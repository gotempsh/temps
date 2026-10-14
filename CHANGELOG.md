# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-beta.57] - 2026-10-11

### Added

- **proxy:** Serve third-party callbacks on a public-only ingress surface
- **email:** Resend captured emails

### Documentation

- **security:** Use david@temps.sh as the vulnerability report address ([#1382](https://github.com/gotempsh/temps/issues/1382))
- **adr:** Add ADR-051 public ingress surface for third-party callbacks

### Fixed

- **web:** Show real load errors, stop doomed requests, and fix console a11y failures ([#1380](https://github.com/gotempsh/temps/issues/1380))
- **deployments:** Single route_generation writer, root directory checked on save, worker Vite build coverage ([#1381](https://github.com/gotempsh/temps/issues/1381))
- QA retest findings — S3 errors, Elixir umbrellas, screenshot hang, HA replica race, pg_dump temp dir, failover after CP outage ([#1395](https://github.com/gotempsh/temps/issues/1395))
- **compose:** Show why a Compose deployment failed, and pass project variables to builds ([#1397](https://github.com/gotempsh/temps/issues/1397))
- QA retest findings #1389–#1394 (DB startup wait, operation status, Drop ZIPs, node placement, HA standby protection, console a11y) ([#1398](https://github.com/gotempsh/temps/issues/1398))
- **web:** Keep the requested page when paginating service backups
- **web:** Keep the plugins tab in the URL
- **web:** Show honest email domain states for SMTP and project access
- **web:** Let dev-server requests pass same-origin guards
- **email:** Keep failed resends resendable and never show a paused SMTP domain as sending
- **email:** Refuse resending an email whose stored headers can no longer be sent
- **email:** Keep a resend's claim when its provider result could not be saved
- **email:** Fence resends so a stalled attempt cannot deliver or overwrite
- **email:** Return a rejected resend to captured when its result cannot be saved
- **deployments:** Pass build-argument values to worker builds for every preset
- **deployments:** Record only platform-set values as public build arguments
- **deployer:** Join continued Dockerfile lines the way Docker does
- **proxy:** Keep gate-denied requests off the MCP endpoint in dual-listener mode
- **config:** Refuse a public ingress hostname that any application uses
- **web:** Never show the console origin as a callback URL
- **config:** Keep the public ingress hostname out of the preview namespace
- **agents:** Skip the microsandbox SDK on Intel macOS so the release binary builds

### Miscellaneous

- **release:** @temps-sdk/cli 0.1.37 + analytics SDKs, repin skills ([#1396](https://github.com/gotempsh/temps/issues/1396))

## [0.1.0-nightly.20261010.47d472a3f] - 2026-10-10

### Added

- **agents:** Add experimental microsandbox sandbox backend ([#1323](https://github.com/gotempsh/temps/issues/1323))
- **nodes:** Dedicated nodes that only run environments pinning them ([#1341](https://github.com/gotempsh/temps/issues/1341))
- **security:** Let admins trust private networks for outbound requests ([#1363](https://github.com/gotempsh/temps/issues/1363))

### CI

- Skip Rust jobs and reuse main's binary when no Rust input changed ([#1361](https://github.com/gotempsh/temps/issues/1361))

### Documentation

- Correct CLI and API examples to match real commands and routes ([#1365](https://github.com/gotempsh/temps/issues/1365))

### Fixed

- **web:** Explain invalid ZIP uploads and allow replacement ([#1335](https://github.com/gotempsh/temps/issues/1335))
- **providers:** Reject PostgreSQL upgrades to the selected image ([#1340](https://github.com/gotempsh/temps/issues/1340))
- **web:** Make console setup and documentation links actionable ([#1336](https://github.com/gotempsh/temps/issues/1336))
- **drop:** Build Rsbuild output from uploaded source ([#1343](https://github.com/gotempsh/temps/issues/1343))
- **deployments:** Retain context for nested workspace apps ([#1342](https://github.com/gotempsh/temps/issues/1342))
- **web:** Honest read-failure states, slug-based telemetry setup link, and commit-pinned redeploys ([#1348](https://github.com/gotempsh/temps/issues/1348))
- **web:** Move the drop delivery provider into a compact summary row ([#1358](https://github.com/gotempsh/temps/issues/1358))
- **proxy:** Answer 503 for stopped apps instead of the console ([#1360](https://github.com/gotempsh/temps/issues/1360))
- **storage:** Keep RustFS starting until authenticated S3 readiness and explain failed initialization ([#1362](https://github.com/gotempsh/temps/issues/1362))
- Preserve path on domain redirects, HTTPS redirect behind proxies, unbiased web-vitals beacons and p75 API ([#1359](https://github.com/gotempsh/temps/issues/1359))
- **qa:** Restore static rollback and safe storage recovery ([#1364](https://github.com/gotempsh/temps/issues/1364))
- **deployer:** Keep BuildKit cache mounts per project on worker builds ([#1372](https://github.com/gotempsh/temps/issues/1372))
- **agent:** Resync routes within seconds after the control plane stops refusing a node ([#1374](https://github.com/gotempsh/temps/issues/1374))
- **providers:** Stop HA clusters by member and report pre-upgrade backup failures ([#1366](https://github.com/gotempsh/temps/issues/1366))
- **templates:** Pin the curated observability starter image by digest ([#1375](https://github.com/gotempsh/temps/issues/1375))
- **docs:** Drop the /howto/ segment from two guide links ([#1377](https://github.com/gotempsh/temps/issues/1377))
- **deployments:** Report take_screenshot as pending until a real image is captured ([#1376](https://github.com/gotempsh/temps/issues/1376))
- **deployments:** Move uploaded-source apps on drain, account for worker containers on node removal, build nested Go/Cargo apps ([#1367](https://github.com/gotempsh/temps/issues/1367))
- **deployments:** Base image retention on what the builder recorded ([#1378](https://github.com/gotempsh/temps/issues/1378))
- **providers:** Roll back cluster lock transactions when a locked check fails ([#1379](https://github.com/gotempsh/temps/issues/1379))

## [0.1.0-nightly.20261009.909788014] - 2026-10-09

### Added

- **providers:** Import data from external databases into managed services ([#1306](https://github.com/gotempsh/temps/issues/1306))
- **deployments:** Let projects and environments build on worker nodes ([#1321](https://github.com/gotempsh/temps/issues/1321))

### Fixed

- **migrations:** Lift TimescaleDB decompression cap for monitor reconciliation ([#1307](https://github.com/gotempsh/temps/issues/1307))
- **deps:** Upgrade next to 16.3.8 and multidict to 6.9.1 ([#1312](https://github.com/gotempsh/temps/issues/1312))
- Console honesty fixes for SMTP tests, backups card, health alerts, image pull failures, RustFS diagnostics ([#1313](https://github.com/gotempsh/temps/issues/1313))
- **drop:** Deploy Deno, Phoenix, plain PHP and Swift starters; qualify Dockerfile and Spring starters ([#1322](https://github.com/gotempsh/temps/issues/1322))
- **otel:** Bound every ClickHouse read and make facets discoverable ([#1325](https://github.com/gotempsh/temps/issues/1325))
- **providers:** Release the Redis database a failed import created ([#1324](https://github.com/gotempsh/temps/issues/1324))
- **security:** Close #1210 follow-ups for cluster app auth and sandbox git tokens ([#1333](https://github.com/gotempsh/temps/issues/1333))

### Testing

- **providers:** Restart legacy SCRAM cluster members like production ([#1305](https://github.com/gotempsh/temps/issues/1305))

## [0.1.0-nightly.20261008.dad133bdd] - 2026-10-08

### Added

- **deployments:** Deploy static sites built on worker nodes ([#1299](https://github.com/gotempsh/temps/issues/1299))

### Fixed

- **providers:** Make control-plane HA members reachable from worker nodes ([#1294](https://github.com/gotempsh/temps/issues/1294))
- **git:** Refresh and check new GitHub App installations ([#1296](https://github.com/gotempsh/temps/issues/1296))
- Close v0.1.0 GA milestone issues ([#1295](https://github.com/gotempsh/temps/issues/1295))
- **otel:** Read global traces page-first from local ClickHouse ([#1297](https://github.com/gotempsh/temps/issues/1297))
- **web:** Preserve AI gateway setup errors and retry context ([#1298](https://github.com/gotempsh/temps/issues/1298))

## [0.1.0-nightly.20261007.2c1f296bc] - 2026-10-07

### CI

- Force incremental builds in the musl cache pruning test ([#1278](https://github.com/gotempsh/temps/issues/1278))

### Fixed

- **release:** Integrate v0.1.0 hardening and qualification gates ([#1274](https://github.com/gotempsh/temps/issues/1274))
- **release:** Keep waiting when the runs API drops a queued test run ([#1275](https://github.com/gotempsh/temps/issues/1275))
- **restore:** Stream restore inputs, scope dump files per attempt, document restore 409s ([#1276](https://github.com/gotempsh/temps/issues/1276))
- **web:** Render Git connection selector with standard tabs ([#1277](https://github.com/gotempsh/temps/issues/1277))
- **web:** Repair console workflows for navigation, settings, backups, alerting and failures ([#1291](https://github.com/gotempsh/temps/issues/1291))
- **upgrade:** Make the pre-migration backup opt-in and reap orphaned dumps ([#1286](https://github.com/gotempsh/temps/issues/1286))
- Resolve open v0.1.0 issues across proxy, cron, alerting, presets and console ([#1293](https://github.com/gotempsh/temps/issues/1293))

## [0.1.0-nightly.20261006.e70c36a7] - 2026-10-06

### Added

- **cloud-protocol:** Console-proxy capability and frame family (ADR-045)
- **cloud-client:** In-process console-proxy worker (ADR-045 WP-B)
- **cloud:** Console-access switch, managed Cloud OIDC provider and role gate (ADR-045 WP-C)
- **cloud:** Start the console-proxy worker from the Cloud plugin (ADR-045)
- **web,cli:** Console-access switch, Cloud login copy and managed-provider guards (ADR-045)
- **upgrade:** Back up migrations, guard schemas and test rollback ([#1227](https://github.com/gotempsh/temps/issues/1227))
- **settings:** Disclose and control anonymous telemetry at runtime ([#1229](https://github.com/gotempsh/temps/issues/1229))
- **deployments:** Explain failures and configure readiness deadlines ([#1228](https://github.com/gotempsh/temps/issues/1228))
- **ai-gateway:** Support OpenAI Responses and batch workflows ([#1242](https://github.com/gotempsh/temps/issues/1242))

### CI

- Enforce clippy and run frontend quality checks ([#1230](https://github.com/gotempsh/temps/issues/1230))
- **first-run:** Nightly first-run suite and quiet-logs soak ([#1218](https://github.com/gotempsh/temps/issues/1218))

### Documentation

- **adr:** 045 console access through Cloud
- **cloud:** Align generated consent guidance with enrollment
- **tutorials:** Sync the deploy guides with the published site ([#1244](https://github.com/gotempsh/temps/issues/1244))
- **tutorials:** Note Rails static files and Django collectstatic in recent builds ([#1267](https://github.com/gotempsh/temps/issues/1267))

### Fixed

- **cloud:** Fence console access and preserve feature consent
- **auth:** Fence OIDC MFA sessions against provider revocation
- **auth:** Bind OIDC challenges and sessions to their issuing provider
- **cloud:** Honor console consent during provisioning and wake relay promptly
- **cloud:** Preserve relay backoff after short connections
- **cloud:** Protect relay headers and keep cancellation responsive
- **deployments:** Preserve usable Git metadata in Docker builds ([#1223](https://github.com/gotempsh/temps/issues/1223))
- **monitoring:** Avoid false alarms on fresh installations ([#1225](https://github.com/gotempsh/temps/issues/1225))
- **onboarding:** Clarify service changes, charts and unavailable commands ([#1232](https://github.com/gotempsh/temps/issues/1232))
- **providers:** Require SCRAM authentication for HA Postgres infrastructure ([#1233](https://github.com/gotempsh/temps/issues/1233))
- **builds:** Upgrade Autopack to v0.1.4
- **deployments:** Protect other instances during container cleanup ([#1234](https://github.com/gotempsh/temps/issues/1234))
- **security:** Bind restores, limit credentials and parameterize metrics ([#1231](https://github.com/gotempsh/temps/issues/1231))
- **drop:** Detect Ruby, PHP and TanStack Start server applications ([#1263](https://github.com/gotempsh/temps/issues/1263))
- **observability:** Bound recurring failure logs and report recovery ([#1235](https://github.com/gotempsh/temps/issues/1235))
- **release:** Gate publishing, separate channels, and verify upgrades ([#1221](https://github.com/gotempsh/temps/issues/1221)) [**BREAKING**]
- **deps:** Drop vulnerable opentelemetry_sdk 0.30 and repair the web ESLint job ([#1265](https://github.com/gotempsh/temps/issues/1265))
- **presets:** Pass project variables to Autopack build steps ([#1266](https://github.com/gotempsh/temps/issues/1266))
- **notifications:** Show the project slug instead of raw IDs in Slack alerts ([#1268](https://github.com/gotempsh/temps/issues/1268))
- **nodes:** Harden the WireGuard mesh, node pairing and SSH enrollment ([#1260](https://github.com/gotempsh/temps/issues/1260))
- **restore:** Recover interrupted restores and explain restore and backup failures ([#1273](https://github.com/gotempsh/temps/issues/1273))

### Miscellaneous

- **deps-dev:** Bump typescript from 6.0.2 to 7.0.2 in /web ([#1247](https://github.com/gotempsh/temps/issues/1247))

### Performance

- **builds:** Accelerate Next.js builds and report BuildKit timings ([#1226](https://github.com/gotempsh/temps/issues/1226))

## [0.1.0-nightly.20261005.5f4906f4] - 2026-10-05

### Added

- **nodes:** Add managed WireGuard mesh and SSH enrollment ([#1186](https://github.com/gotempsh/temps/issues/1186))
- **ai:** Simplify harness setup and connect OpenCode to OpenAI-compatible APIs ([#1187](https://github.com/gotempsh/temps/issues/1187))
- **web:** One-click first deploy and a clearer Quick Start for new installs ([#1220](https://github.com/gotempsh/temps/issues/1220))

### Documentation

- Fill GA documentation gaps and tidy repository hygiene ([#1219](https://github.com/gotempsh/temps/issues/1219))

### Fixed

- **security:** Patch Dependabot alerts and Trivy container-scan findings ([#1191](https://github.com/gotempsh/temps/issues/1191))
- First-run blockers — Flexible image deploys, docker context, job-log 500s and log noise ([#1192](https://github.com/gotempsh/temps/issues/1192))
- **console:** Explain startup failures without interrupting apps ([#1224](https://github.com/gotempsh/temps/issues/1224))
- **deployments:** Always show build logs, with HTTP fallback and dev WebSocket proxy ([#1217](https://github.com/gotempsh/temps/issues/1217))

## [0.1.0-nightly.20261004.32a8e9c1] - 2026-10-04

### Added

- **dns:** Ownership-guarded managed DNS records + Cloudflare proxied mode (ADR-031) ([#347](https://github.com/gotempsh/temps/issues/347))
- **secrets:** Add credential checks, local expiry checks and history to secrets ([#1188](https://github.com/gotempsh/temps/issues/1188))

### CI

- Skip build and test jobs on documentation-only pull requests ([#1182](https://github.com/gotempsh/temps/issues/1182))

### Documentation

- **upgrade:** Move old Compose stacks' PostgreSQL data into the volume ([#1180](https://github.com/gotempsh/temps/issues/1180))

### Fixed

- **proxy:** Serve a static site's own 404.html instead of the SPA shell ([#1184](https://github.com/gotempsh/temps/issues/1184))
- **nodes:** Stop node removal deadlocking with the DNS publisher ([#1185](https://github.com/gotempsh/temps/issues/1185))

### Testing

- **e2e:** Multi-node over WireGuard between isolated networks, and fix the worker overlay MTU ([#1189](https://github.com/gotempsh/temps/issues/1189))
- **web:** Include local_artifacts in the mocked env var detection response ([#1190](https://github.com/gotempsh/temps/issues/1190))

## [0.1.0-nightly.20261003.6f74680e] - 2026-10-02

### Added

- **sandbox:** Run sandboxes on worker nodes (ADR-048) ([#1173](https://github.com/gotempsh/temps/issues/1173))

### Fixed

- Bugs reported from a production fork (error grouping, CLI login, OTLP listener, retention, SDK) ([#1175](https://github.com/gotempsh/temps/issues/1175))

## [0.1.0-nightly.20261002.2d6c7780] - 2026-10-02

### Added

- Simplify onboarding and support External projects ([#1153](https://github.com/gotempsh/temps/issues/1153))

### Fixed

- Patch web audit advisories and Rust 1.99 clippy lints ([#1176](https://github.com/gotempsh/temps/issues/1176))
- **presets:** Honor pnpm configuration and detect pnpm in React App ([#1177](https://github.com/gotempsh/temps/issues/1177))
- **presets:** Do not follow symlinks when scanning a repository ([#1178](https://github.com/gotempsh/temps/issues/1178))

## [0.1.0-nightly.20260930.53d4fb40] - 2026-09-29

### Documentation

- **request-flow:** Describe the proxy pipeline as implemented ([#1170](https://github.com/gotempsh/temps/issues/1170))

### Fixed

- **cli:** Exit non-zero when resource flags fail validation ([#1168](https://github.com/gotempsh/temps/issues/1168))
- **deployments:** Set PORT to the port detected from the image ([#1169](https://github.com/gotempsh/temps/issues/1169))
- **config:** Return on_demand_tls from GET /settings and pin a lossless settings round-trip ([#1172](https://github.com/gotempsh/temps/issues/1172))

## [0.1.0-nightly.20260929.95fdac25] - 2026-09-29

### Documentation

- **multi-node:** Describe least-loaded scheduling and current limitations ([#1157](https://github.com/gotempsh/temps/issues/1157))

### Fixed

- **cli:** Offer stopped deployments in the rollback picker ([#1163](https://github.com/gotempsh/temps/issues/1163))
- **cli:** Convert CPU flags to the API's microcore units ([#1162](https://github.com/gotempsh/temps/issues/1162))

## [0.1.0-nightly.20260928.f6f763e0] - 2026-09-28

### Added

- **compose:** Allow several public URLs per Compose service ([#1152](https://github.com/gotempsh/temps/issues/1152))

### Fixed

- Network creation race, import DB URLs and Coolify values, Visit domain, previews-off deploys ([#1154](https://github.com/gotempsh/temps/issues/1154))

## [0.1.0-nightly.20260927.d30d578d] - 2026-09-27

### Added

- **telemetry:** Report operation outcomes and failures ([#1148](https://github.com/gotempsh/temps/issues/1148))

### Fixed

- **proxy:** Join HTTP/2 cookie crumbs into one Cookie header upstream ([#1149](https://github.com/gotempsh/temps/issues/1149))
- **email:** Verify Scaleway domains against the DKIM record Scaleway checks ([#1150](https://github.com/gotempsh/temps/issues/1150))

### Testing

- **web:** Give Cloud telemetry e2e fixtures the delivery fields ([#1151](https://github.com/gotempsh/temps/issues/1151))

## [0.1.0-nightly.20260926.ed69c4e6] - 2026-09-26

### Added

- **deployments:** Build source images on worker nodes ([#1146](https://github.com/gotempsh/temps/issues/1146))

### Documentation

- **rollbacks,scaling:** Fix fabricated CLI commands, wrong defaults, and a CLI unit bug ([#1130](https://github.com/gotempsh/temps/issues/1130))

### Fixed

- **telemetry:** Use a random persisted telemetry ID for stateless control planes ([#1145](https://github.com/gotempsh/temps/issues/1145))
- **otel:** Alert on Cloud delivery failures only while they are happening ([#1147](https://github.com/gotempsh/temps/issues/1147))

## [0.1.0-nightly.20260926.de626de6] - 2026-09-26

### CI

- Stop the Actions cache from growing past what builds use ([#1142](https://github.com/gotempsh/temps/issues/1142))

### Fixed

- **ci:** Replace withdrawn MinIO images with RustFS and the rc client ([#1137](https://github.com/gotempsh/temps/issues/1137))
- **ui:** Compact env and log views with rolling time ranges ([#1140](https://github.com/gotempsh/temps/issues/1140))
- **deployments:** Stream uploaded images to remote nodes instead of pulling ([#1139](https://github.com/gotempsh/temps/issues/1139))
- **logs:** Replay every WAL generation the writer produces and never pause collection on one it cannot ([#1132](https://github.com/gotempsh/temps/issues/1132))
- **monitoring:** Stop crash alarms for superseded deployments' containers and add bulk alarm actions ([#1138](https://github.com/gotempsh/temps/issues/1138))
- **telemetry-api:** Restore country geolocation and backfill unknown countries ([#1144](https://github.com/gotempsh/temps/issues/1144))

### Testing

- **e2e:** Align console specs with rolling time presets and compose filters ([#1143](https://github.com/gotempsh/temps/issues/1143))

## [0.1.0-nightly.20260925.9edf44ab] - 2026-09-25

### Added

- **git:** Git connection detail page with paginated repositories ([#1134](https://github.com/gotempsh/temps/issues/1134))
- **auth:** Let admins reset another user's password ([#1078](https://github.com/gotempsh/temps/issues/1078)) ([#1135](https://github.com/gotempsh/temps/issues/1135))

### Fixed

- **config:** Preserve cluster CA during settings saves ([#1115](https://github.com/gotempsh/temps/issues/1115))
- **monitoring:** Reconcile worker ports safely ([#1118](https://github.com/gotempsh/temps/issues/1118))
- **deployments:** Support remote promote and rollback ([#1122](https://github.com/gotempsh/temps/issues/1122))
- **status-page:** Probe managed monitors through local proxy ([#1119](https://github.com/gotempsh/temps/issues/1119))
- **git:** Authenticate public repository deployments ([#1128](https://github.com/gotempsh/temps/issues/1128))
- **database:** Log SQL statements at TRACE, keep slow-query warnings ([#1129](https://github.com/gotempsh/temps/issues/1129))
- **logs:** Collect only owned containers, name facets across the range; size provider logo ([#1133](https://github.com/gotempsh/temps/issues/1133))
- **proxy:** Stop serving gzip bytes as text/markdown ([#1136](https://github.com/gotempsh/temps/issues/1136))

## [0.1.0-nightly.20260924.68c44d50] - 2026-09-24

### Documentation

- **admin-listener:** Move AI gateway to the admin route surface ([#1092](https://github.com/gotempsh/temps/issues/1092))
- **security:** Remove fabricated content, fix attack-mode and log retention claims ([#1109](https://github.com/gotempsh/temps/issues/1109))

### Fixed

- **proxy:** Route project wildcard domains and wake sleeping environments ([#1103](https://github.com/gotempsh/temps/issues/1103))
- **console:** Hide sparse checkout option at repository root ([#1102](https://github.com/gotempsh/temps/issues/1102))
- **compose:** Make policy checks individually actionable ([#1093](https://github.com/gotempsh/temps/issues/1093))
- **startup:** Bound WAL recovery and decouple proxy from console initialization ([#1104](https://github.com/gotempsh/temps/issues/1104))
- **backup:** Use engine-specific exact WAL-G retention deletion ([#1105](https://github.com/gotempsh/temps/issues/1105))
- **compose:** Return complete preview problem details ([#1106](https://github.com/gotempsh/temps/issues/1106))
- **backup:** Type WAL-G deletion identity ([#1107](https://github.com/gotempsh/temps/issues/1107))
- **dns:** Keep cluster DNS available without the console ([#1110](https://github.com/gotempsh/temps/issues/1110))
- **deployer:** Report safe Compose config diagnostics ([#1112](https://github.com/gotempsh/temps/issues/1112))
- **cloud:** Expose disconnect recovery for rejected credentials ([#1111](https://github.com/gotempsh/temps/issues/1111))
- **logs:** Recover WAL without blocking console startup ([#1108](https://github.com/gotempsh/temps/issues/1108))
- **docker:** Run release binaries in a compatible glibc runtime ([#1113](https://github.com/gotempsh/temps/issues/1113))
- **cli:** Fail static deployment waits on permanent status errors ([#1114](https://github.com/gotempsh/temps/issues/1114))
- **env:** Make preview variable scope explicit and inherited ([#1116](https://github.com/gotempsh/temps/issues/1116))

### Testing

- **e2e:** Wait for asynchronously created default monitor ([#1094](https://github.com/gotempsh/temps/issues/1094))

## [0.1.0-nightly.20260923.a7bfeb68] - 2026-09-23

### Fixed

- **compose:** Resolve remote references and reset policies safely ([#1090](https://github.com/gotempsh/temps/issues/1090))
- **logs:** Make global and runtime logs readable on mobile ([#1091](https://github.com/gotempsh/temps/issues/1091))

## [0.1.0-nightly.20260923.9b9e5b45] - 2026-09-23

### Added

- **control-plane:** Support replaceable stateless installations ([#1081](https://github.com/gotempsh/temps/issues/1081))
- **git:** Optionally clone only the project root directory ([#1067](https://github.com/gotempsh/temps/issues/1067))
- **cloud:** Reconnect installations to an existing cloud identity ([#1088](https://github.com/gotempsh/temps/issues/1088))

### Fixed

- **backup:** Report WAL-G per-backup size ([#1086](https://github.com/gotempsh/temps/issues/1086))
- **deployer:** Preflight Docker Compose availability ([#1085](https://github.com/gotempsh/temps/issues/1085))
- **logs:** Purge unsealed head-buffer lines, not just sealed chunks ([#1082](https://github.com/gotempsh/temps/issues/1082))
- **logs:** Preserve legacy chunks during v2 migration ([#1087](https://github.com/gotempsh/temps/issues/1087))
- **git:** Terminate sparse clone helpers on cancellation ([#1089](https://github.com/gotempsh/temps/issues/1089))

## [0.1.0-nightly.20260922.c728ab64] - 2026-09-22

### Added

- **monitoring:** Add automatic credential checks and variable history ([#1072](https://github.com/gotempsh/temps/issues/1072))
- **analytics:** Let deployment tokens enrich their own project's visitors ([#1076](https://github.com/gotempsh/temps/issues/1076))
- **ingress:** Serve public app traffic through workers ([#1077](https://github.com/gotempsh/temps/issues/1077))
- **logs:** Object-store log chunks with ClickHouse line index (ADR-046/047) ([#1064](https://github.com/gotempsh/temps/issues/1064))
- **deployer:** Host Docker socket grant for operator-owned infrastructure services (ADR 045) ([#1068](https://github.com/gotempsh/temps/issues/1068))

### Fixed

- **ci:** Retire the stale nextest target seed that breaks the docker-tests archive ([#1079](https://github.com/gotempsh/temps/issues/1079))
- **deployments:** Keep failure reports within the central endpoint's size limit ([#1080](https://github.com/gotempsh/temps/issues/1080))

### Miscellaneous

- **deps-dev:** Bump typescript from 6.0.2 to 7.0.2 in /web ([#1070](https://github.com/gotempsh/temps/issues/1070))

## [0.1.0-nightly.20260921.3bd97125] - 2026-09-20

### Added

- **cloud:** TEMPS_CLOUD_BACKEND_URL one-shot bootstrap for unattended enrollment ([#1061](https://github.com/gotempsh/temps/issues/1061))
- **auth:** Cloud-managed OIDC provider with instance role gate ([#1055](https://github.com/gotempsh/temps/issues/1055))
- **auth:** Cloud console-access OIDC bootstrap file + fixed managed-login start ([#1063](https://github.com/gotempsh/temps/issues/1063))
- **compose:** Add per-project security policy exceptions ([#1057](https://github.com/gotempsh/temps/issues/1057))

### Fixed

- **ui:** Restore log counts sidebar and use provider logos ([#1056](https://github.com/gotempsh/temps/issues/1056))
- **otel:** Bound Cloud trace reads and restore AI activity ([#1058](https://github.com/gotempsh/temps/issues/1058))
- **serve:** Let --profile control-plane boot without a Docker socket ([#1062](https://github.com/gotempsh/temps/issues/1062))
- **control-plane:** Run without Docker, ask for a worker node instead of failing ([#1059](https://github.com/gotempsh/temps/issues/1059))
- **backup:** Fail a backup this process cannot run instead of parking it ([#1066](https://github.com/gotempsh/temps/issues/1066))

### Testing

- **web:** Fix 9 failing e2e specs on main ([#1065](https://github.com/gotempsh/temps/issues/1065))

## [0.1.0-nightly.20260920.760cda68] - 2026-09-19

### Added

- **plugins:** Install from subdirectories and select update refs ([#1045](https://github.com/gotempsh/temps/issues/1045))
- Unattended Cloud enrollment env var + source-free release compose ([#1044](https://github.com/gotempsh/temps/issues/1044))
- **analytics:** Add guided AI visitor activity reports ([#1039](https://github.com/gotempsh/temps/issues/1039))
- **plugins:** Improve install review and show live progress ([#1049](https://github.com/gotempsh/temps/issues/1049))
- **ui:** Polish settings, observability, and domain management ([#1050](https://github.com/gotempsh/temps/issues/1050))
- **plugins:** Simplify running rows and standardize embedded workspaces ([#1054](https://github.com/gotempsh/temps/issues/1054))

### Fixed

- **web:** Align time-range tests and remove nested settings gutter ([#1047](https://github.com/gotempsh/temps/issues/1047))
- **env:** Keep marked secrets write-only ([#1046](https://github.com/gotempsh/temps/issues/1046))
- **web:** Restore navigation and plugin E2E coverage ([#1051](https://github.com/gotempsh/temps/issues/1051))
- **nodes:** Wait out a grace period before failing over an offline node ([#1053](https://github.com/gotempsh/temps/issues/1053))

## [0.1.0-nightly.20260919.317c141e] - 2026-09-19

### Added

- **error-tracking:** Resolve tunneled Sentry envelopes from the DSN; Retry-After on keyed ingest
- **serve:** Control-plane profile that runs no local workloads ([#1031](https://github.com/gotempsh/temps/issues/1031))
- **geo:** Keep MaxMind GeoLite2 database and IP lookups fresh ([#1036](https://github.com/gotempsh/temps/issues/1036))
- **cli:** Add local plugin runner with fixtures and event testing ([#1035](https://github.com/gotempsh/temps/issues/1035))
- **logs:** Archive finished build/deploy logs to S3-compatible storage ([#1040](https://github.com/gotempsh/temps/issues/1040))
- **deployer,file-store,proxy:** S3-compatible static-site and CAS storage with a hot-path byte cache ([#1041](https://github.com/gotempsh/temps/issues/1041))
- **ds:** Codify console design system and migrate reference pages ([#1037](https://github.com/gotempsh/temps/issues/1037))
- **cloud-protocol,cloud-client:** Instance status reporting (ADR-039) ([#1042](https://github.com/gotempsh/temps/issues/1042))

### Documentation

- **skills:** Guide plugin authors through design, testing and publishing ([#1038](https://github.com/gotempsh/temps/issues/1038))

### Fixed

- **scheduling:** Due-based monitors, persisted alert breach state, due-only cron scans
- **auth:** Correct middleware layer order in auth rate limiter ([#1043](https://github.com/gotempsh/temps/issues/1043))

## [0.1.0-nightly.20260918.e006bb00] - 2026-09-17

### Added

- **plugins:** Add host-brokered AI with persistent actors and grants ([#1029](https://github.com/gotempsh/temps/issues/1029))

### Fixed

- **deployments:** Update stale MockDatabase fixture in multinode integration test ([#1028](https://github.com/gotempsh/temps/issues/1028))
- **web:** Resolve React lint errors and warnings ([#1027](https://github.com/gotempsh/temps/issues/1027))
- **monitoring:** Show names/slugs instead of raw numeric IDs in alert notifications ([#1030](https://github.com/gotempsh/temps/issues/1030))

## [0.1.0-nightly.20260917.b823a663] - 2026-09-17

### Documentation

- **sdks:** Document externally hosted SPA ingest topology (ADR-040) ([#1023](https://github.com/gotempsh/temps/issues/1023))

### Fixed

- **deployments:** Redeploy images by deployment source ([#1016](https://github.com/gotempsh/temps/issues/1016))
- **ai:** Diagnose unreachable credential relays ([#1018](https://github.com/gotempsh/temps/issues/1018))
- **preview:** Restore sandbox routes after gateway restart ([#1022](https://github.com/gotempsh/temps/issues/1022))
- **analytics:** Count identified visitors, not sessions, for active-visitors badge ([#1024](https://github.com/gotempsh/temps/issues/1024))
- **deployments:** Prevent failover redeploy storms ([#1019](https://github.com/gotempsh/temps/issues/1019))
- **deployer:** Detect out-of-memory build steps and report build caps honestly ([#1025](https://github.com/gotempsh/temps/issues/1025))

### Performance

- **observability:** Speed traces and continue log searches ([#1017](https://github.com/gotempsh/temps/issues/1017))

## [0.1.0-nightly.20260916.33eb6be3] - 2026-09-16

### Added

- **proxy:** Add request authorization extension hook ([#1007](https://github.com/gotempsh/temps/issues/1007))
- **cli:** Add SSH setup proof of concept ([#1005](https://github.com/gotempsh/temps/issues/1005))
- **ai:** Add harness preflight checks and consented smoke tests ([#1012](https://github.com/gotempsh/temps/issues/1012))
- **backup:** Record and show until when each backup is retained ([#1011](https://github.com/gotempsh/temps/issues/1011))
- **console:** Let extensions add links to the Settings sidebar ([#1015](https://github.com/gotempsh/temps/issues/1015))

### Fixed

- **plugins:** Restore "verified" wording dropped in repository-install refactor ([#1009](https://github.com/gotempsh/temps/issues/1009))
- **proxy:** Sanitize the password-wall redirect and rate-limit password guesses ([#1006](https://github.com/gotempsh/temps/issues/1006))
- **deployments:** Clean up legacy failed containers ([#1008](https://github.com/gotempsh/temps/issues/1008))
- **ai:** Report actionable credential verification failures ([#1010](https://github.com/gotempsh/temps/issues/1010))
- **ai:** Preserve subscription relay protocol headers ([#1014](https://github.com/gotempsh/temps/issues/1014))
- **cli:** Make local image uploads resilient ([#1013](https://github.com/gotempsh/temps/issues/1013))

## [0.1.0-nightly.20260915.03f32acc] - 2026-09-15

### Fixed

- **ci:** Populate shared daemon image build caches ([#1004](https://github.com/gotempsh/temps/issues/1004))

## [0.1.0-nightly.20260914.88203997] - 2026-09-14

### Added

- **proxy:** Allow admins to control loopback forwarded IP trust
- **config:** Audit loopback forwarded-IP trust changes
- **cli:** Automate TypeScript plugin publishing ([#978](https://github.com/gotempsh/temps/issues/978))

### Documentation

- **config:** Drop stale CLI/env override mentions from settings model

### Fixed

- **workspaces:** Gate releases and guide harness setup
- **logs:** Render ansi colors and remove page volume chart
- **workspaces:** Verify credentials and recover managed runtimes
- **workspaces:** Verify saved logins and preserve harness sessions
- **workspaces:** Verify both image architectures and type mcp
- **config:** Preserve credentials across concurrent settings saves
- **proxy:** Preserve forwarded client IPs from loopback proxies
- **proxy:** Select the final forwarded header without iteration
- **proxy:** Make local forwarded IP trust opt-in
- **proxy:** Allow forwarded IP trust through environment
- **proxy:** Retain forwarded IP trust on settings read failure
- **proxy:** Release refresh snapshot before polling sleep
- **proxy:** Load forwarded IP trust before serving
- **proxy:** Honor startup trust override without settings read
- **proxy:** Remove env/CLI override for loopback forwarded-IP trust
- **proxy:** Recognize IPv4-mapped IPv6 loopback peers
- **console:** Restore responsive deployment list items ([#983](https://github.com/gotempsh/temps/issues/983))
- **cli:** Drop the api.temps.sh relay default from temps join ([#984](https://github.com/gotempsh/temps/issues/984))
- **environments:** Stop gating plaintext env-var reveal behind SecretsRead ([#1002](https://github.com/gotempsh/temps/issues/1002))
- **web:** Expose AI crawler time ranges and scoped counts ([#1000](https://github.com/gotempsh/temps/issues/1000))
- **web:** Remove baseUrl for TypeScript 7 compatibility
- **web:** Type-check TypeScript 7 upgrade
- **release:** Pin runtime images before building release binaries ([#1003](https://github.com/gotempsh/temps/issues/1003))

### Miscellaneous

- **deps-dev:** Bump typescript from 5.9.2 to 7.0.2 in /web

### Testing

- **proxy:** Align mock listener fixture with main

## [0.1.0-nightly.20260914.ec1f1ee1] - 2026-09-14

### Added

- **console:** Add sortable analytics table columns

### Documentation

- Clarify Rust and frontend test placement

### Fixed

- **web:** Restore environment variables in project menu
- **ci:** Separate Trivy SARIF reporting from severity gate
- **deps:** Patch Next.js example and vulnerable transitive packages
- **ci:** Pin container scan image digest across Trivy passes
- **plugins:** Filter catalog by server platform
- **console:** Simplify deployment list structure
- **console:** Sort error groups by displayed range counts
- **backup:** Close the control-plane dump's schema-only tables over foreign keys
- **backup:** Fail the control-plane backup when the FK closure cannot be resolved
- **console:** Use full width page and form layouts
- **console:** Avoid duplicate settings gutters and test form panels
- **analytics:** Upsert visitor on session-replay init lookup miss
- **analytics:** Cap visitor creation from session-replay init
- **analytics:** Bound visitor id length on session-replay init upsert
- **analytics:** Address Greptile findings on session-replay visitor upsert
- **analytics:** Per-IP visitor-creation sub-limit, validate at entry
- **analytics:** Fix visitor-creation limiter check order and bound its IP map
- **analytics:** Shard visitor-creation IP limiter with DashMap

### Testing

- **console:** Verify backup form page widths
- **analytics:** Use generic referrer fixture in backfill test

## [0.1.0-nightly.20260913.5c9d477f] - 2026-09-13

### Added

- **sandbox:** Run commands through resident runtime daemon
- **cloud:** Create the nightly backup schedule for the managed destination
- **cloud:** Name the services whose archive is pinned elsewhere, default MariaDB schedules to Cloud
- **cloud:** Declare large in-place snapshots by manifest object
- **workspaces:** Integrate persistent runtimes and image releases
- **plugins:** Install signed registry binaries
- **plugins:** Add rotating registry trust chain

### CI

- Never restore web/node_modules from another lockfile's cache

### Fixed

- **web:** Reference console-kit as a workspace dependency, not file:
- **web,ci:** Drop the stale js-yaml devDependency, install from an exact cache key
- **cloud:** Release schedules on disconnect, never guess retention, keep the backup plugin optional
- **cloud:** Disable schedules atomically before deleting, document ensure responses
- **providers:** Switch MinIO images from Docker Hub to quay.io
- **providers:** Pin remaining test-fixture MinIO image to quay.io
- **providers:** Normalize persisted MinIO images to quay.io on load
- **e2e:** Accept singular detected preset
- **e2e:** Seed console project and update analytics selectors
- **logs:** Retain partial results when search reaches its limit
- **cloud:** Scope archive conflicts to the schedule's targets, keep existing MariaDB pins
- **backup:** Resume multipart uploads instead of aborting on a failed part
- **cloud:** Bound manifest reads, list sentinels by delimiter, keep typed upload errors
- **e2e:** Align observability specs with current console state
- **web:** Give global project scope picker an accessible name
- **traces:** Restore cross-project waterfall entry point
- **plugins:** Close registry review findings
- **plugins:** Close registry merge blockers
- **plugins:** Gate management controls by role
- **web:** Unify React types and patch js-yaml
- **plugins:** Refresh registry trust on demand
- **web:** Enforce a single React runtime
- **plugins:** Select artifacts by Linux libc
- **plugins:** Persist revocations and surface lifecycle failures

### Miscellaneous

- **sandbox:** Make runtime verification scripts executable

### Testing

- **e2e:** Assert selected project name inside picker
- **e2e:** Target scoped project picker after reload
- **plugins:** Prove candidate process transitions
- **plugins:** Exercise protocol v2 install flow

## [0.1.0-nightly.20260912.c0b03c64] - 2026-09-11

### Added

- **metrics:** Control-plane host I/O counters, latest-sample and docker disk usage endpoints
- **cli:** Server status, metrics and docker-disk-usage commands
- **web:** Control-plane Server page on the operator design system
- **console:** Unify navigation, observability, and database linking
- **cloud:** Declare managed backups by size, WAL-G repositories in place, report deletions

### Fixed

- **captcha-wasm:** Rebuild artifact for CI's Linux amd64 toolchain
- **console:** Unify layouts and polish settings workflows
- **metrics:** Count only hardware NICs and whole disks; URL range, pause and synced cursor on the Server page
- **deployments:** Measure the local Docker daemon, no DOCKER_HOST; page-level retry covers every metric query
- **cloud:** Re-declare a hashed copy when Cloud ignores in_place_root

### Miscellaneous

- **console:** Add required source attribution headers

### Refactor

- **web:** Rebuild the Server page in the current console design

### Testing

- **backup:** Cover retention and refused-queue BackupDeleted publication

## [0.1.0-nightly.20260911.194e154f] - 2026-09-11

### Added

- **cloud:** Catalog managed-source native snapshots in place

### Fixed

- **secrets:** Allow matching keys in separate environments
- **secrets:** Serialize environment deletion with scoped writes
- **monitors:** Edit check paths and sync compose health routes
- **backup:** Fail s3_mirror on Access Denied during comparison listing
- **backup:** Detect Access Denied streaming, not from a bounded tail
- **backup:** Enumerate RustFS buckets instead of whole-account mc mirror
- **backup:** Paginate bucket enumeration and fix per-bucket size prefix
- **cloud:** Resume interrupted backup mirror passes instead of restarting them
- **backup:** Keep the stderr watch verdict when the log drain times out
- **backup:** Fail closed when a watched stderr stream cannot be fully inspected
- **backup:** Treat any uninspected stderr as a failed watch, not only a slow drain
- **backup:** Name the backup and service in the s3_mirror access-denied error
- **captcha-wasm:** Reproduce committed package on Linux amd64
- **ci:** Stop blocking builds on captcha WASM byte differences
- **cloud:** Fall back to the copy path when Cloud rejects an in-place root
- **cloud:** Keep the copy path for WAL-G stream snapshots and type the stub DTOs

## [0.1.0-nightly.20260910.8322f2dc] - 2026-09-09

### Added

- **cloud-protocol:** Report the source database's actual Docker image
- **observability:** Add global logs traces errors and analytics endpoints

### Fixed

- **captcha-wasm:** Resync committed pkg/ with pinned-toolchain rebuild
- **compose:** Stop injecting a default CPU limit
- **web:** Map catalog-only preset slugs before requesting managed env vars
- **cloud:** Use a static human-readable source label for control-plane backups
- **deployer:** Grant baseline capabilities to every sandboxed Compose service
- **observability:** Disclose trace cutovers and filter hosting-provider speed samples
- **git:** Report the repository's actual default branch, not "main"
- **git:** Fetch default branch live instead of trusting the DB cache
- **git:** Keep branch listing resilient and sync remaining is_default clients
- **git:** Self-heal cached default branch and fix TS narrowing regression
- **web:** Let live is_default win over a stale defaultBranch prop
- **git:** Make default-branch persistence a single atomic UPDATE
- **git:** Don't gate the branch-cache fast path on a live provider call

### Testing

- **web:** Colocate provided-environment-variables tests with their module

## [0.1.0-nightly.20260909.b31a7b64] - 2026-09-09

### Added

- **ai:** Refresh provider model catalogs on demand

### Fixed

- **cloud:** Wake the backup mirror sweep on local backup completion
- **status-page:** Adopt existing environment monitors
- **migrations:** Preserve newer monitor paths on rollback
- **migrations:** Distinguish monitor path changes
- **status-page:** Track monitor path revisions
- **cloud:** Mirror control-plane backups instead of rejecting them as non-WAL-G
- **agent:** Bind published container ports to the node's private address, not 0.0.0.0
- **agent:** Reject dangerous private_address overrides (0.0.0.0, loopback, etc.)
- **agent:** Normalize private_address override to a bare IP before binding
- **deployments:** Stop truncating bare IPv6 private_address values
- **deployments,cli:** Normalize private_address end-to-end, not just at agent startup
- **routes,cli:** Bracket IPv6 private addresses when combined with a port
- **cli:** Parse --agent-address as a real SocketAddr, not split(':')

## [0.1.0-nightly.20260908.5135508b] - 2026-09-08

### Added

- **web:** Add @temps-sdk/op operator design-system package
- **design-system:** Add the v5 design-system sandbox, guide and docs
- **design-system:** Kind icons wherever they add context
- **web:** Fmt helpers, Field errors, FormErrors, chart legend and table view, design tokens file
- **web:** Date, time, range, duration and schedule fields
- **web:** Agent, data-viz primitives, running state and night tokens
- **design-system:** Logs explorer across applications
- **web:** Skin scrollbars
- **design-system:** A copy answers on the control, never in a toast
- **design-system:** Landing header links to the Temps Cloud status page
- **design-system:** Resources monitoring on nodes and services
- **design-system:** The URL is the state
- **design-system:** Prose classes and the Article template
- **cloud:** Add secure managed telemetry client
- **cloud:** Add managed cloud link lifecycle and console settings
- **console:** Find Temps Cloud from the command palette
- **cloud:** Integrate managed alert routing
- **cloud:** Declare backup restore artifacts
- **cloud:** Stream managed backups and AI access
- **cloud:** Report instance location metadata
- **multi-node:** Initialize and rotate cluster CA safely
- **security:** Harden cluster CA rotation
- **agent:** Add systemd service installer
- **cloud:** Auto-provision, rotate, and audit the managed backup credential
- **web:** Badge and guard Cloud-managed S3 sources in the UI
- **cloud-client:** Cloud telemetry read proxy client (spike, ADR-040)
- **otel:** ADR-040 Phase A - cloud telemetry fidelity tiers and backfill
- **otel:** ADR-041 - Cloud-primary telemetry writes
- **otel,cloud:** ADR-042 one-click Cloud telemetry activation
- **cloud:** Thread session tokens through managed backup credentials
- **cloud:** Make ClickHouse entities and backups cloud-primary (ADR-043)
- **cloud-protocol:** Add backup lifecycle event wire types ([#912](https://github.com/gotempsh/temps/issues/912))
- **cloud:** Push backup lifecycle events to Cloud and webhooks
- **web:** Prototype AI-first onboarding workspace
- **web:** Model multi-project AI application stacks
- **ai:** Add application-first workspace
- **ai:** Start applications with a harness thread
- **ai:** Complete persistent workspace workflow
- **ai:** Add sandbox harness maintenance commands
- **ai:** Add persistent workspace file explorer
- **ai:** Add workspace file import and transfer
- **ai:** Add bounded workspace file transfers
- **providers:** Expose each service's continuous-archive S3 source

### Build

- **mariadb:** Publish WAL-G image through GHCR

### Documentation

- **agents:** Add the temps-design-system skill and point CLAUDE.md and AGENTS.md at it
- **design-system:** Content, forms, notifications, data viz, tokens, motion, icons and locale guidelines
- **design-system:** Dates, times and ranges in forms
- **design-system:** Generative UI, data viz forms, build-a-screen gallery, night rules
- **design-system:** Calendar heatmap section shows its demo
- **design-system:** Plugin UI, publish-ready package, review fixes

### Fixed

- **design-system:** One mark before a project name
- **design-system:** Drop the density command from the palette demo
- **design-system:** Session timeline axis draws inset ticks, not glyphs
- **auth:** Track pending-MFA-session origin so SSO enforcement can tell it apart from password login ([#918](https://github.com/gotempsh/temps/issues/918))
- **proxy:** Fall back to root index.html for asset-only directories ([#926](https://github.com/gotempsh/temps/issues/926))
- **email:** Scaleway domain verification, opt-in delete, and provider domain import picker ([#925](https://github.com/gotempsh/temps/issues/925))
- **migrations:** Stop blanket-demoting managed-monitor ownership ([#927](https://github.com/gotempsh/temps/issues/927))
- **openapi:** Preserve canonical cloud schema diff
- **cloud:** Validate backup upload destinations
- **backups:** Make MariaDB WAL-G PITR E2E self-contained
- **cloud:** Harden managed data paths
- **cloud:** Preserve safe startup across restarts
- **cloud:** Persist in-flight telemetry across restarts
- **cloud:** Close security review findings
- **redis:** Report restore startup crash details
- **backups:** Reject deletion of retained sources
- **backups:** Strip Redis replication trailer
- **backups:** Pull digest-pinned sidecars
- **network:** Auto-detect underlay device instead of hardcoding eth0
- **e2e:** Provision immutable MariaDB recovery image
- **proxy:** Lower console fallback log severity
- **agent:** Derive overlay MTU from underlay
- **dns:** Avoid host resolver bind collision
- **network:** Join control plane to multi-node overlay
- **network:** Escape nftables baseline marker
- **network:** Enforce authoritative cluster CIDR
- **network:** Permit scoped VXLAN forwarding
- **network:** Match Docker VXLAN ingress path
- **network:** Gate control-plane kernel lifecycle test
- **cloud:** Restore cross-node services and MariaDB backups
- **network:** Preserve cross-node service return paths
- **network:** Reconcile upgraded firewall baselines
- **multi-node:** Finalize secure cluster onboarding
- **deployments:** Stop compose containers when deploy job fails
- **dns:** Prevent duplicate internal answers
- **multinode:** Route container lifecycle to owning worker
- **cloud-client:** Surface the real error while telemetry is dropping
- **cli:** Include temps-cloud crates in the server log level filter
- **config:** Preserve ADR-041/042 cloud fields not sent by every client
- **cloud-client:** Attribute health to a block reason instead of a placeholder
- **cloud:** Close credential-redaction and encoding gaps in session-token support
- **cloud:** Surface Cloud's actual reason instead of a generic message
- **otel:** Match Cloud-primary span reads to the ts DateTime64 column
- **cloud:** Keep Cloud's reason when a retryable backup call fails
- **cloud:** Route pg_dump-fallback Postgres backups to native mirror, not WAL-G ([#901](https://github.com/gotempsh/temps/issues/901))
- **backup:** Skip S3 lifecycle reconcile for unscheduled operator buckets ([#896](https://github.com/gotempsh/temps/issues/896))
- **cloud:** Stop one stuck backup from throttling Cloud reporting for all others ([#906](https://github.com/gotempsh/temps/issues/906))
- **providers:** Pin WAL-G archive source per Postgres service
- **providers:** Extend continuous archive source pinning to MariaDB
- **providers:** Stop health-monitor full-model save from reverting the continuous-archive pin
- **providers:** Close IDOR and two ordering bugs in archive repoint ([#907](https://github.com/gotempsh/temps/issues/907))
- **providers:** Restore safe default MariaDB image instead of hard-failing
- **cloud:** Tighten credential redaction in backup lifecycle notify
- **cloud:** Remove dead capability advertisement and unread snapshot field
- **otel:** Remove dead cloud-telemetry outbox code
- **network:** Remove dead code from the multi-node overlay subsystem
- **cloud:** Decrypt STS session token before building S3 client credentials
- **webhooks:** Require BackupsRead when a webhook's backup-event subscription persists across an update
- **backup:** Shell-escape PGDATA-derived paths interpolated into sh -c commands
- **backup:** Scope external-service backup capability to accessible services
- **network:** Drop only the offending peer instead of stalling all control-plane reconciliation
- **notifications:** Require write permission to send a test notification
- **cloud-client:** Pin backup uploads to Cloudflare R2 endpoints
- **cli:** Add two missing leaf commands to the audited command-tree inventory
- **migrations:** Derive rollback step count instead of hardcoding it
- **backup:** Make MariaDB PITR tests and the pitr-scenario request the WAL-G image
- **providers,web:** Add MariaDB docker_image schema default and surface silent form validation failures
- **ci:** Align mariadb-walg-image's setup-qemu-action pin with the rest of the repo
- **deps:** Update hickory-net to 0.26.2 for hickory-resolver 0.26.2 compat
- **web:** Pin MariaDB managed preset to the real published WAL-G image digest
- **web:** Activate AI conversation options
- **ai:** Harden application onboarding
- **web:** Clarify AI project planning
- **tooling:** Preserve typo checker allowlist
- **ai:** Close application workspace review findings
- **sandbox:** Enforce secure cross-platform egress
- **ai:** Deserialize list pagination directly
- **ai:** Route sandbox model relay securely
- **ai:** Open managed sandbox status
- **ai:** Relay sandbox MCP capabilities securely
- **ai:** Close workspace review findings
- **ci:** Repair AI workspace regression gates
- **ai:** Isolate sandbox preview authentication
- **ai:** Paginate only visible conversations
- **ai:** Preserve archived conversation pages
- **cloud:** Mirror Redis/MongoDB logical-fallback backups to Cloud
- **cloud:** Address Greptile review findings on the redis/mongo mirror fix
- **providers:** Default WAL health probe's host to localhost, add CLI command
- **backups:** Accept MariaDB WAL-G repository bases for PITR restores
- **e2e:** Keep DinD bridges out of the cluster compute pool
- **agents:** Support secure sandbox networks on older Docker

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#919](https://github.com/gotempsh/temps/issues/919))
- **merge:** Sync cloud funnel with main
- **api:** Regenerate Cloud integration clients
- **cloud:** Regenerate clients after main merge
- Add source attribution headers
- **web,cli:** Regenerate API clients and skill docs after merge
- **ai:** Retrigger PR checks

### Refactor

- **design-system:** Name the first published system v1
- **design-system:** One chrome for the guide and the reference pages
- **design-system:** Drop the plan and density knobs from the console header
- **design-system:** UseUrlState moves into @temps-sdk/op
- **design-system:** Rename the package to @temps-sdk/ds
- **design-system:** Point imports, docs and the lockfile at @temps-sdk/ds
- **cloud:** Require backup checksums
- **deployments:** Centralize container runtime operations

### Testing

- **cloud:** Prove bounded shutdown flush
- **backups:** Prove MariaDB PITR with immutable images
- **cloud:** Fix migration and degraded-state regressions
- **redis:** Stabilize restart-loop regression
- **e2e:** Stabilize command and preview journeys
- **network:** Preserve existing control-plane routes
- **ci:** Synchronize websocket capture assertion

## [0.1.0-nightly.20260907.647e4043] - 2026-09-06

### Added

- **deployments:** Diagnose Docker Hub rate limits, add registry mirror prefix ([#905](https://github.com/gotempsh/temps/issues/905))
- **proxy:** Support real client IPs behind Bunny CDN ([#917](https://github.com/gotempsh/temps/issues/917))

### Fixed

- **error-tracking:** Honor Sentry envelope event ID ([#911](https://github.com/gotempsh/temps/issues/911))
- **proxy:** Stop classifying HTTP 101 responses as errors ([#913](https://github.com/gotempsh/temps/issues/913))
- **presets:** Accept full 2xx range in Next.js container health poll ([#914](https://github.com/gotempsh/temps/issues/914))

## [0.1.0-nightly.20260906.48c2396e] - 2026-09-06

### Fixed

- **cli:** Repair management commands and issue 899 regressions ([#904](https://github.com/gotempsh/temps/issues/904))
- **agents:** Surface preview gateway failures ([#909](https://github.com/gotempsh/temps/issues/909))
- **email:** Fix Scaleway domain identity bugs, add domain import ([#902](https://github.com/gotempsh/temps/issues/902))

## [0.1.0-nightly.20260905.e6ab76fc] - 2026-09-04

### Added

- **web:** Make delete-confirmation name chip copyable ([#900](https://github.com/gotempsh/temps/issues/900))
- **projects:** Install services from templates ([#855](https://github.com/gotempsh/temps/issues/855))

### Fixed

- **services:** Preserve template and monitor ownership ([#903](https://github.com/gotempsh/temps/issues/903))

## [0.1.0-nightly.20260904.44d70f90] - 2026-09-03

### Added

- **vulnerability-scanner:** Make Trivy scanning opt-in per project, disabled by default ([#892](https://github.com/gotempsh/temps/issues/892))

### Fixed

- **console:** Streamline service actions and log onboarding ([#890](https://github.com/gotempsh/temps/issues/890))
- **backup:** Harden restore safety and health UX ([#889](https://github.com/gotempsh/temps/issues/889))
- **backups:** Create scoped schedules atomically ([#891](https://github.com/gotempsh/temps/issues/891))
- **backup:** Use a GUC-compatible recovery_target_time format for PITR ([#894](https://github.com/gotempsh/temps/issues/894))
- **ai-agent-cli:** Use constant-time comparison for MCP bridge token ([#893](https://github.com/gotempsh/temps/issues/893))
- **console:** Unify navigation and responsive page chrome ([#895](https://github.com/gotempsh/temps/issues/895))

## [0.1.0-nightly.20260903.31b49635] - 2026-09-02

### Added

- **backup:** Implement MariaDB backup + point-in-time recovery
- **routes:** Add live Traefik docker-label discovery, with opt-in TLS for discovered routes ([#857](https://github.com/gotempsh/temps/issues/857))

### Fixed

- **web:** Pin browserslist to patched version to clear bun audit ([#882](https://github.com/gotempsh/temps/issues/882))
- **backup:** Add native disaster recovery restore ([#881](https://github.com/gotempsh/temps/issues/881))
- **web:** Send comparator symbols when creating an alert rule ([#884](https://github.com/gotempsh/temps/issues/884))
- **web:** Send symbolic comparator values for service alert rules ([#883](https://github.com/gotempsh/temps/issues/883))
- **web:** Harden dogfood-reported UI bugs across alert rules, storage, and env vars ([#885](https://github.com/gotempsh/temps/issues/885))
- **deployments:** Honor configured port over image EXPOSE detection ([#887](https://github.com/gotempsh/temps/issues/887))
- **deployments:** Allow developers to cancel and pause their own deployments ([#886](https://github.com/gotempsh/temps/issues/886))
- **console:** Align deployment state and service UX ([#888](https://github.com/gotempsh/temps/issues/888))

### Performance

- **otel:** Bound relay forwarding memory ([#880](https://github.com/gotempsh/temps/issues/880))

## [0.1.0-nightly.20260902.6abf301e] - 2026-09-01

### Added

- **runtime:** Add Docker Compose deployment ([#841](https://github.com/gotempsh/temps/issues/841))
- **analytics:** Key-based ingest for apps Temps does not deploy (ADR-040) ([#865](https://github.com/gotempsh/temps/issues/865))
- **skills:** Support platform provisioning device login ([#875](https://github.com/gotempsh/temps/issues/875))

### Fixed

- **web:** Stale deploy status badge and Load Balancer heading mismatch ([#871](https://github.com/gotempsh/temps/issues/871))
- **presets:** Two autopack Dockerfile-generation bugs found while building jupyterlab/jupyterlab ([#769](https://github.com/gotempsh/temps/issues/769))
- **web:** Persist Docker registry settings ([#877](https://github.com/gotempsh/temps/issues/877))

### Testing

- **proxy:** Raise captcha PoW attempt cap to stop random CI flakes ([#872](https://github.com/gotempsh/temps/issues/872))

## [0.1.0-nightly.20260901.e9dcea78] - 2026-09-01

### Styling

- **otel:** Restyle pipeline status charts to match analytics/Metrics Explorer ([#869](https://github.com/gotempsh/temps/issues/869))

## [0.1.0-nightly.20260901.27cadb1d] - 2026-08-31

### Added

- **presets:** Detect Dockerfile exposed ports ([#856](https://github.com/gotempsh/temps/issues/856))
- **deployments:** Deploy static-preset git repos with zero container overhead ([#858](https://github.com/gotempsh/temps/issues/858))

### Fixed

- **projects:** Address project creation review findings ([#853](https://github.com/gotempsh/temps/issues/853))
- **ai-chat:** Discover page country analytics ([#868](https://github.com/gotempsh/temps/issues/868))
- **otel:** Support proto3 optional fields in release builds ([#867](https://github.com/gotempsh/temps/issues/867))
- **deployments:** Bound disk use during static-from-source deployment copy ([#866](https://github.com/gotempsh/temps/issues/866))

## [0.1.0-nightly.20260831.11f3e324] - 2026-08-30

### Added

- **deployments:** Filter and limit environment container metrics by deployment ([#843](https://github.com/gotempsh/temps/issues/843))
- **projects:** Improve database and environment setup ([#840](https://github.com/gotempsh/temps/issues/840))
- **cli:** Add services metrics CLI parity ([#850](https://github.com/gotempsh/temps/issues/850))

### Fixed

- **cli:** Replace Bun-only runtime APIs with Node equivalents ([#842](https://github.com/gotempsh/temps/issues/842))
- **web:** Remove redundant deployment success toasts ([#845](https://github.com/gotempsh/temps/issues/845))
- **git:** Support self-hosted GitLab repository discovery ([#846](https://github.com/gotempsh/temps/issues/846))
- **web:** Stop login form from resetting while the user is typing ([#844](https://github.com/gotempsh/temps/issues/844))
- **sandbox:** Preserve snapshot workspaces across backends ([#839](https://github.com/gotempsh/temps/issues/839))
- **proxy:** Fail fast with an actionable error when port 80/443 are already in use ([#847](https://github.com/gotempsh/temps/issues/847))
- **agent:** Bound worker memory during remote operations ([#851](https://github.com/gotempsh/temps/issues/851))
- **deployer:** Pull compose images before force-recreate teardown ([#852](https://github.com/gotempsh/temps/issues/852))

### Testing

- **cli:** Cover services metrics validation, reject non-positive IDs ([#854](https://github.com/gotempsh/temps/issues/854))

## [0.1.0-nightly.20260830.9a2637ec] - 2026-08-29

### Fixed

- **release:** Pin Linux release builds to ubuntu-22.04 for glibc compat ([#837](https://github.com/gotempsh/temps/issues/837))
- **auth:** Validate password character count instead of byte length ([#834](https://github.com/gotempsh/temps/issues/834))
- **web:** Trim api key name on creation and prevent double-submit ([#832](https://github.com/gotempsh/temps/issues/832))
- **monitoring:** Suppress crash/downtime alerts for paused deployments ([#835](https://github.com/gotempsh/temps/issues/835))
- **auth:** Don't treat transient DB errors as an expired session ([#838](https://github.com/gotempsh/temps/issues/838))

## [0.1.0-nightly.20260829.643d4315] - 2026-08-28

### Miscellaneous

- **cli:** Repin temps skills to @temps-sdk/cli@0.1.35 ([#836](https://github.com/gotempsh/temps/issues/836))

## [0.1.0-nightly.20260828.24861e9b] - 2026-08-27

### Added

- **notifications:** Add severity-based notification routes ([#822](https://github.com/gotempsh/temps/issues/822))

### Fixed

- **build:** Install python3 in the musl toolchain image ([#825](https://github.com/gotempsh/temps/issues/825))
- **docker:** Apk upgrade base packages in runtime images ([#829](https://github.com/gotempsh/temps/issues/829))
- **proxy:** Require Upgrade-Insecure-Requests for HTTP-origin page tracking ([#830](https://github.com/gotempsh/temps/issues/830))

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#823](https://github.com/gotempsh/temps/issues/823))

## [0.1.0-nightly.20260827.e265afab] - 2026-08-27

### Added

- **web:** Add URL-backed project pagination ([#818](https://github.com/gotempsh/temps/issues/818))

### Fixed

- **cli:** Close staged binary before self-update preflight ([#819](https://github.com/gotempsh/temps/issues/819))
- **ai-gateway:** Scope usage summary/by-provider to the caller ([#817](https://github.com/gotempsh/temps/issues/817))
- **cli:** Read options.filter instead of options.filters in api-query ([#821](https://github.com/gotempsh/temps/issues/821))

### Miscellaneous

- **legal:** Preserve source attribution in distributions ([#820](https://github.com/gotempsh/temps/issues/820))

## [0.1.0-nightly.20260827.a78b7ec0] - 2026-08-26

### Added

- **mcp:** Rebuild Temps as an MCP server (ADR-039)
- **agent,deployments,cli:** Harden cluster DNS and surface resolver health for troubleshooting ([#799](https://github.com/gotempsh/temps/issues/799))
- **otel:** Harden ingest pipeline reliability and observability ([#815](https://github.com/gotempsh/temps/issues/815))

### Fixed

- **status-page:** Delay first health check cycle to avoid proxy-startup race ([#814](https://github.com/gotempsh/temps/issues/814))
- **web:** Restore dark-mode border/input contrast for toggles and buttons ([#813](https://github.com/gotempsh/temps/issues/813))
- **providers:** Keep MongoDB credentials off mongosh's command line ([#816](https://github.com/gotempsh/temps/issues/816))
- **providers:** Stop sending unreachable connection strings to cross-node services ([#795](https://github.com/gotempsh/temps/issues/795))

### Performance

- **geo:** Memory-map the MaxMind databases instead of reading them into the heap ([#808](https://github.com/gotempsh/temps/issues/808))
- **proxy:** Stop spawning a thread and a runtime per background loop ([#809](https://github.com/gotempsh/temps/issues/809))
- **logs:** Locate the tail replay window by scanning backwards in 64 KB blocks ([#807](https://github.com/gotempsh/temps/issues/807))

## [0.1.0-nightly.20260826.0004c042] - 2026-08-25

### Added

- **cli:** Default temps setup to sslip.io when no wildcard domain is given ([#790](https://github.com/gotempsh/temps/issues/790))

### Fixed

- **domains:** Disable Pebble nonce fault injection ([#796](https://github.com/gotempsh/temps/issues/796))
- **proxy:** Preserve original IP in admin gate updates ([#805](https://github.com/gotempsh/temps/issues/805))
- **console:** Show tooltips in collapsed sidebar ([#806](https://github.com/gotempsh/temps/issues/806))
- **providers:** Make external service operations node-aware ([#810](https://github.com/gotempsh/temps/issues/810))
- **proxy:** Fail closed on unresolvable IP when a restriction policy exists ([#811](https://github.com/gotempsh/temps/issues/811))
- **proxy:** Harden static file serving and ingestion ([#812](https://github.com/gotempsh/temps/issues/812))
- **auth:** Close cross-tenant project-scoping gaps in five handlers ([#797](https://github.com/gotempsh/temps/issues/797))

### Miscellaneous

- **docker:** Bump musl builder toolchain from rust:1.94-alpine to rust:1.98-alpine

### Performance

- **cli:** Stream the upgrade download and extraction to disk ([#804](https://github.com/gotempsh/temps/issues/804))

## [0.1.0-nightly.20260825.c754dd77] - 2026-08-24

### Added

- **web:** Add latency/traffic tabs to API traffic chart ([#792](https://github.com/gotempsh/temps/issues/792))

### Fixed

- **deps:** Replace unmaintained get_if_addrs and document accepted RUSTSEC risks ([#778](https://github.com/gotempsh/temps/issues/778))
- **web:** Detect static-site environments by source_type, not preset ([#777](https://github.com/gotempsh/temps/issues/777))
- **screenshots:** Stop plugin registration blocking on Chrome availability check ([#789](https://github.com/gotempsh/temps/issues/789))
- **cli:** Read upgrade's --license-path from TEMPS_EE_LICENSE_PATH too ([#791](https://github.com/gotempsh/temps/issues/791))
- **web:** Stop environment metrics legend from listing every replaced container ([#793](https://github.com/gotempsh/temps/issues/793))
- **providers:** Retry transient Docker image pull failures ([#794](https://github.com/gotempsh/temps/issues/794))
- **auth:** Require MFA step-up + confirmation on destructive/sensitive actions ([#788](https://github.com/gotempsh/temps/issues/788))

## [0.1.0-nightly.20260824.ce99d856] - 2026-08-24

### Fixed

- **deployments:** Don't gate deploy completion on DNS ACK when cluster DNS is off ([#776](https://github.com/gotempsh/temps/issues/776))

## [0.1.0-nightly.20260824.29b81382] - 2026-08-23

### Fixed

- **web:** Render live password policy checklist in Account.tsx ([#768](https://github.com/gotempsh/temps/issues/768))
- **web:** Render accessible live password policy checklist in ResetPassword ([#762](https://github.com/gotempsh/temps/issues/762))

## [0.1.0-nightly.20260823.0ddf1660] - 2026-08-23

### Fixed

- **routes:** Close lost-wakeup race in route sync long-poll ([#763](https://github.com/gotempsh/temps/issues/763))

## [0.1.0-nightly.20260823.ed49a729] - 2026-08-22

### Fixed

- **web:** Handle fetch errors in EmailProviderDetail with retry state ([#758](https://github.com/gotempsh/temps/issues/758))
- **web:** Trim whitespace inputs and guard against double-submission in CreateS3Source ([#760](https://github.com/gotempsh/temps/issues/760))

## [0.1.0-nightly.20260822.02fb4bd8] - 2026-08-21

### Added

- **projects:** Allow renaming a project's display name
- **proxy:** Add an optional per-project/environment IP access extension point
- **proxy:** Let a standalone proxy enforce project IP rules too

### Fixed

- **projects:** Make rename atomic, audit the persisted name, reset form on project switch
- **projects:** Roll back port-change writes when the route reload cannot be signalled
- **projects:** Fail closed when a route-reload NOTIFY cannot be published
- **projects:** Report the rename from the service instead of inferring it
- **projects:** Keep the rename read-back inside the transaction that wrote it
- **projects:** Correct rollback claims, share name validation, gate Docker tests
- **routes:** Bound route-table staleness when the LISTEN connection dies silently
- **proxy:** Close the allow-all startup race on project_ip_gate_slot
- **ci:** Satisfy cargo fmt on PR #725
- **proxy:** Close the project-ip-gate startup race by construction, not a timeout

### Miscellaneous

- Satisfy clippy 1.98 lints across the workspace

### Refactor

- **projects:** Address review findings on the rename change

### Styling

- **proxy:** Satisfy cargo fmt in the IP gate slot handoff

### Testing

- **projects:** Pin the partial-update semantics the git-settings path relies on
- **proxy:** Register the IP gate slot in the plugin-registration test

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
