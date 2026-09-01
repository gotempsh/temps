#!/usr/bin/env bun
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Command } from 'commander'
import { getProjects } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from './lib/client.ts'
import { loadCommand } from './commands/load.ts'
import { scenarioCommand } from './commands/scenario.ts'
import { examplesCommand } from './commands/examples.ts'
import { tlsScenarioCommand } from './commands/tls-scenario.ts'
import { dns01WildcardScenarioCommand } from './commands/dns01-wildcard-scenario.ts'
import { emailScenarioCommand } from './commands/email-scenario.ts'
import { cliScenarioCommand } from './commands/cli-scenario.ts'
import { kvScenarioCommand } from './commands/kv-scenario.ts'
import { blobScenarioCommand } from './commands/blob-scenario.ts'
import { flagsScenarioCommand } from './commands/flags-scenario.ts'
import { backupRestoreScenarioCommand } from './commands/backup-restore-scenario.ts'
import { pitrScenarioCommand } from './commands/pitr-scenario.ts'
import { auditScenarioCommand } from './commands/audit-scenario.ts'
import { managedServicesScenarioCommand } from './commands/managed-services-scenario.ts'
import { rbacScenarioCommand } from './commands/rbac-scenario.ts'
import { monitoringScenarioCommand } from './commands/monitoring-scenario.ts'
import { errorTrackingScenarioCommand } from './commands/error-tracking-scenario.ts'
import { logsScenarioCommand } from './commands/logs-scenario.ts'
import { analyticsScenarioCommand } from './commands/analytics-scenario.ts'
import { sessionReplayScenarioCommand } from './commands/session-replay-scenario.ts'
import { gitDeployScenarioCommand } from './commands/git-deploy-scenario.ts'
import { dbHaFailoverScenarioCommand } from './commands/db-ha-failover-scenario.ts'
import { deployLifecycleScenarioCommand } from './commands/deploy-lifecycle-scenario.ts'
import { otelQuotaScenarioCommand } from './commands/otel-quota-scenario.ts'
import { s3RestoreScenarioCommand } from './commands/s3-restore-scenario.ts'
import { redisRestoreScenarioCommand } from './commands/redis-restore-scenario.ts'
import { mongodbRestoreScenarioCommand } from './commands/mongodb-restore-scenario.ts'
import { pgUpgradeScenarioCommand } from './commands/pg-upgrade-scenario.ts'
import { mariadbRestoreScenarioCommand } from './commands/mariadb-restore-scenario.ts'
import { envVarsScenarioCommand } from './commands/env-vars-scenario.ts'
import { apiKeyScenarioCommand } from './commands/api-key-scenario.ts'
import { multinodeJoinScenarioCommand } from './commands/multinode-join-scenario.ts'
import { markdownCommand } from './commands/markdown.ts'
import { aiChatPromptsCommand } from './commands/ai-chat-prompts.ts'

const program = new Command()

program
  .name('temps-e2e')
  .description('End-to-end + load testing CLI for a live Temps instance')
  .version('0.1.0')

// Global connection options (also read from TEMPS_URL / TEMPS_API_KEY).
program
  .option('--url <url>', 'Temps instance base URL (default: $TEMPS_URL or http://localhost:8080)')
  .option('--api-key <key>', 'API key (default: $TEMPS_API_KEY)')

function connection(): { url?: string; apiKey?: string } {
  const o = program.opts<{ url?: string; apiKey?: string }>()
  return { url: o.url, apiKey: o.apiKey }
}

program
  .command('ping')
  .description('Verify connectivity + auth against the instance')
  .option('--json', 'machine-readable output')
  .action(async (opts: { json?: boolean }) => {
    const client = makeClient(resolveConfig(connection()))
    const data = unwrap(
      await getProjects({ client, query: { page: 1, per_page: 1 } }),
      'getProjects',
    ) as { total?: number }
    if (opts.json) {
      process.stdout.write(JSON.stringify({ ok: true, projects: data.total ?? 0 }) + '\n')
    } else {
      process.stdout.write(`OK — connected. ${data.total ?? 0} project(s) visible.\n`)
    }
  })

program
  .command('ai-chat-prompts')
  .description(
    'Print the provider-neutral AI chat acceptance prompts, expected tool actions, assertions, and cleanup steps',
  )
  .option('--category <category>', 'filter by read, service, linking, routing, domain, permissions, or runtime')
  .option('--destructive-only', 'show only prompts that create or change infrastructure')
  .option('--json', 'machine-readable prompt catalog')
  .action(aiChatPromptsCommand)

program
  .command('load')
  .description('Generate HTTP load against a URL (no Temps deploy required)')
  .argument('<url>', 'target URL to hammer')
  .option('-n, --requests <n>', 'total requests to send', '1000')
  .option('-c, --concurrency <n>', 'max in-flight requests', '50')
  .option('-d, --duration <dur>', 'run for a duration instead of a fixed count (e.g. 60s, 2m)')
  .option('-m, --method <method>', 'HTTP method', 'GET')
  .option('-H, --header <header...>', 'request header "Key: Value" (repeatable)')
  .option('--timeout <ms>', 'per-request timeout in ms')
  .option('--json', 'machine-readable output')
  .action(loadCommand)

program
  .command('scenario')
  .description('Full lifecycle: project -> deploy image -> wait healthy -> load -> verify -> teardown')
  .option('--image <ref>', 'public Docker image to deploy', 'ghcr.io/temps-sh/e2e-hello:latest')
  .option('--port <port>', 'container port the image listens on', '80')
  .option('-n, --requests <n>', 'load requests after deploy', '2000')
  .option('-c, --concurrency <n>', 'load concurrency', '50')
  .option('--with-db', 'also provision a postgres service')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await scenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('tls-scenario')
  .description(
    'Deploy an app, provision a real TLS certificate via Pebble (HTTP-01), and verify it is served correctly',
  )
  .option('--image <ref>', 'public Docker image to deploy', 'ghcr.io/temps-sh/e2e-hello:latest')
  .option('--port <port>', 'container port the image listens on', '80')
  .option('--tls-port <port>', 'the target instance TLS listener port to dial', '8443')
  .option('--tls-host <host>', 'the target instance TLS listener host to dial', '127.0.0.1')
  .option('--pebble-network <name>', 'docker network pebble-challtestsrv is on', 'temps-e2e-pebble-net')
  .option('--pebble-management-url <url>', 'pebble-challtestsrv management API URL', 'http://localhost:8056')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await tlsScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('dns01-wildcard-scenario')
  .description(
    'Register a Pebble DNS provider, issue a wildcard certificate via a real DNS-01 challenge, and verify it',
  )
  .option('--pebble-management-url <url>', 'pebble-challtestsrv management API URL', 'http://localhost:8056')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await dns01WildcardScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('email-scenario')
  .description(
    'Create an SMTP provider pointed at Mailpit, send a tracked email, and verify real receipt + open/click tracking',
  )
  .option('--smtp-host <host>', 'Mailpit SMTP host, from the target instance\'s point of view', 'localhost')
  .option('--smtp-port <port>', 'Mailpit SMTP port', '1025')
  .option('--mailpit-url <url>', 'Mailpit web/REST API base URL', 'http://localhost:8025')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await emailScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('cli-scenario')
  .description(
    'Genuine CLI e2e: spawns the real @temps-sdk/cli binary as a subprocess for every step (create/list/show/delete a project, env vars, a service, a deploy polled to a terminal state, a domain, and a minted API key that is then used to authenticate a real call) -- proves argv parsing, command wiring, and stdout formatting, not just the API',
  )
  .option('--image <ref>', 'public Docker image to deploy', 'traefik/whoami:latest')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await cliScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('kv-scenario')
  .description(
    'Real kv-storage data-plane round trip: set/get/incr/expire/ttl/keys/del, nx/xx conditionals, and cross-project tenant isolation -- no console UI exists for this feature beyond an enabled/healthy status badge, so this is the only coverage that exists',
  )
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await kvScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('blob-scenario')
  .description(
    'Real blob-storage (RustFS) data-plane round trip: put/download/head/list/copy/delete, deleted-blob 404, and cross-project tenant isolation',
  )
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await blobScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('flags-scenario')
  .description(
    'Feature flags driven through the real @temps-sdk/node-sdk FlagsClient: defaults, per-environment overrides, the kill switch, ETag/304 caching, and exposure reporting',
  )
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await flagsScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('backup-restore-scenario')
  .description(
    'Backup + restore a real postgres service via MinIO: real wal-g backup-push, real S3 upload, real in-place restore proven by reverting post-backup writes',
  )
  .option('--registry <host:port>', 'registry to push the db-probe image to (or $TEMPS_E2E_REGISTRY)')
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint, reachable from the target instance', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket to store backups in (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await backupRestoreScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('pitr-scenario')
  .description(
    'Point-in-time recovery of a real postgres service via MinIO: real wal-g backup-push, write T1 rows, wait for their WAL to archive, capture a recovery-target timestamp, write T2 rows, PITR-restore to T1, and prove via the read-only data-browser API (not /probe) that exactly T1s rows survive',
  )
  .option('--registry <host:port>', 'registry to push the db-probe image to (or $TEMPS_E2E_REGISTRY)')
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint, reachable from the target instance', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket to store backups in (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await pitrScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('mongodb-restore-scenario')
  .description(
    'MongoDB backup + in-place restore: provision a MongoDB service, insert pre-backup docs via docker exec mongosh, ' +
      'run a real mongodump backup (MongodbEngine sidecar → MinIO), insert post-backup docs, restore in place, ' +
      'and verify via the data-browser API that pre-backup docs are present and post-backup docs are absent',
  )
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await mongodbRestoreScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('git-deploy-scenario')
  .description(
    'Real git-triggered deploy against a public repo (github.com/gotempsh/temps-examples): clone + build a language-preset subdirectory, trigger-pipeline, verify the exact response body baked into the real repo source',
  )
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for the build+deploy to go healthy (real clone + real build)', '600000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await gitDeployScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('audit-scenario')
  .description(
    'Real audit-log round trip: create + delete a project, then read back the exact PROJECT_CREATED/PROJECT_DELETED rows via GET /audit/logs and /audit/logs/{id} (id/data/actor/timestamp match, not just "the list is non-empty"), and confirm the read endpoint itself is RBAC-gated',
  )
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await auditScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('managed-services-scenario')
  .description(
    'Real managed-database round trip: provision + link a postgres service to a project BEFORE deploying, deploy an app that writes through the injected POSTGRES_URL, verify an exact row-count round trip through /probe, verify the resolved-env-vars reveal endpoint, then unlink',
  )
  .option('--registry <host>', 'registry to push the probe image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--probe-requests <n>', 'number of /probe hits (== expected final row count)', '5')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await managedServicesScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('rbac-scenario')
  .description(
    'Real RBAC permission-boundary proof: a second, independently-authenticated low-privilege user is granted team access to a project at viewer/deployer/admin tiers, asserting the exact 200/403 transitions and permission strings the guard enforces -- not just that team/access CRUD returns 2xx',
  )
  .option('--temps-root <path>', 'checkout root (needs crates/temps-cli) to mint a second user\'s bearer key via DB-direct `temps api-key`; or $TEMPS_ROOT')
  .option('--database-url <url>', 'Postgres URL the temps binary can reach directly; or $TEMPS_DATABASE_URL')
  .option('--image <ref>', 'image to deploy for the deployer-role assertion', 'ghcr.io/temps-sh/e2e-hello:latest')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await rbacScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('monitoring-scenario')
  .description(
    'Real uptime-monitoring lifecycle: confirm a fresh project auto-gets a default monitor, create an explicit second one, deploy a toggleable app, wait for the FIXED 60s check cycle to catch a real 5xx outage (raw status "major_outage") and later its recovery, and assert the incident/bucketed-chart/status-overview side effects at each step -- not just that monitor/incident CRUD returns 2xx',
  )
  .option('--registry <host>', 'registry to push the toggle-app image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await monitoringScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('error-tracking-scenario')
  .description(
    'Real Sentry-compatible error-tracking lifecycle: create a DSN, POST real Sentry-shaped events authenticated with the DSN key (not the bearer token), and assert fingerprint-based grouping actually groups repeats and separates distinct exceptions -- not just that the error-group CRUD routes return 2xx',
  )
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await errorTrackingScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('logs-scenario')
  .description(
    'Real log-aggregation lifecycle: deploy an app that emits structured JSON stdout/stderr lines, wait out the actual chunk-flush window, and verify full-text search, level filtering, JSONB `fields` passthrough, env filtering, grep-style context, and purge -- through the real Docker log collector, not synthetic rows',
  )
  .option('--registry <host>', 'registry to push the log-emitter image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await logsScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('analytics-scenario')
  .description(
    'Real web-analytics lifecycle: deploy an HTML app so the proxy actually issues visitor/session cookies, replay them on POST /api/_temps/event the way a real tracking snippet would, and verify event-entries custom-property round-trip plus visitor-journey session stitching -- not just that the query routes return 2xx',
  )
  .option('--registry <host>', 'registry to push the analytics-app image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await analyticsScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('session-replay-scenario')
  .description(
    'Real session-replay lifecycle: deploy an HTML app so the proxy issues a visitor cookie, init a replay session (retrying past the visitor-upsert race), POST base64+zlib event batches, verify the project/visitor list surfaces it once duration is computed, playback returns all events with custom fields intact, a manual duration override sticks, and delete soft-removes it -- not just that the routes return 2xx',
  )
  .option('--registry <host>', 'registry to push the analytics-app image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await sessionReplayScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('db-ha-failover-scenario')
  .description(
    'Real Postgres HA (pg_auto_failover) automatic-failover proof: provision a 1-monitor + 2-data-node cluster on a single Docker host, deploy an app through the injected multi-host POSTGRES_URL, write real rows, `docker stop` the elected primary container, and assert cluster-health promotes the surviving replica AND the same app (no redeploy) resumes writing within a bounded window -- not just that a status field flipped',
  )
  .option('--registry <host>', 'registry to push the probe image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--cluster-timeout <ms>', 'max wait for the cluster to reach a running/steady state', '180000')
  .option('--failover-timeout <ms>', 'max wait for promotion + write recovery after stopping the primary', '90000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await dbHaFailoverScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('redis-restore-scenario')
  .description(
    'Backup + restore a real Redis service via MinIO: real wal-g backup-push into a helper container, real S3 upload, real in-place restore proven by verifying pre-backup keys survive (HTTP 200 from data-browser) and post-backup keys are erased (HTTP 404) -- not just that the API calls returned 2xx',
  )
  .option('--registry <host:port>', 'registry to push the redis-probe image to (or $TEMPS_E2E_REGISTRY)')
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint, reachable from the target instance', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket to store backups in (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await redisRestoreScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('otel-quota-scenario')
  .description(
    'Real OTLP/HTTP protobuf ingestion (traces/metrics/logs, hand-encoded, no @opentelemetry/* SDK) round-tripped through the real query API and the unified Observe feed, then real storage-quota enforcement: push volume past a configured TEMPS_OTEL_QUOTA_GB until GET /otel/quota crosses 100%, and assert ingestion is actually rejected with 413 (and the rejected data never lands) once the ingest-time quota cache re-checks -- not just that the config knob exists',
  )
  .option('--max-quota-batches <n>', 'safety cap on ~9MB batches pushed while hunting for the quota cutoff', '250')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await otelQuotaScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('s3-restore-scenario')
  .description(
    'S3/MinIO managed-service restore: provision a real MinIO service with known credentials, upload pre-backup objects, mirror-backup via mc, diverge (add + delete), restore in-place with --overwrite --remove, and verify via the platform data-browser API that the live bucket exactly matches the backup-time state (deleted object restored, post-backup object gone)',
  )
  .option('--minio-endpoint <url>', 'backup-destination MinIO S3 API endpoint (must already have the bucket)', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'backup-destination bucket (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await s3RestoreScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('pg-upgrade-scenario')
  .description(
    'Real Postgres major-version upgrade (17 → 18): provision a service pinned to gotempsh/postgres-walg:17-bookworm, deploy a db-probe app, write 5 marker rows, trigger the upgrade, poll through all phases to completed, and assert the marker rows survived via the read-only data-browser API (not /probe) -- proves the actual pg_dumpall → psql restore path, not just that the status field flipped',
  )
  .option('--registry <host:port>', 'registry to push the db-probe image to (or $TEMPS_E2E_REGISTRY)')
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint, reachable from the target instance', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket to store backups in (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--upgrade-timeout <ms>', 'max wait for the upgrade to complete (real wal-g backup + pg_dumpall + psql restore)', '600000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await pgUpgradeScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('examples')
  .description('Build the repo example projects (Go, Python, Node, …) and run the full deploy/verify lifecycle for each')
  .option('--only <key...>', 'run only these example keys (repeatable); default = fast subset')
  .option('--all', 'run every registered example (includes heavy Rust/Vite builds)')
  .option('--registry <host>', 'registry to push built images to so Temps can pull them (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--list', 'list available examples and exit')
  .option('-n, --requests <n>', 'load requests per example', '500')
  .option('-c, --concurrency <n>', 'load concurrency', '25')
  .option('--with-db', 'also provision a postgres service per example')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for each deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await examplesCommand({ ...opts, connection: connection() })
  })

program
  .command('deploy-lifecycle-scenario')
  .description(
    'Real deployment lifecycle: deploy version A, deploy version B, rollback to A, pause, resume, and promote B into a second environment -- asserting the EXACT response body served over the real proxied URL at each step, not just a deployment row\'s status field',
  )
  .option('--registry <host>', 'registry to push the versioned-app images to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for each deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await deployLifecycleScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('mariadb-restore-scenario')
  .description(
    'MariaDB backup + in-place restore: provision a MariaDB service, insert pre-backup rows via docker exec mariadb, ' +
      'run a real backup (physical or logical, engine-selected) to MinIO, insert post-backup rows, restore in place, ' +
      'and verify via the data-browser API that pre-backup rows are present and post-backup rows are absent',
  )
  .option('--minio-endpoint <url>', 'MinIO S3 API endpoint, reachable from the target instance', 'http://localhost:9092')
  .option('--minio-bucket <name>', 'MinIO bucket to store backups in (must already exist)', 'temps-e2e-backups')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await mariadbRestoreScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('env-vars-scenario')
  .description(
    'Real environment-variable lifecycle: deploy an echo-server app, create/update/delete a project env var, ' +
      'redeploy after each change, and assert the RUNNING CONTAINER reflects the new/updated/removed value ' +
      '(not just that the API round-trips it) -- plus a lightweight environment-scoping check via the list endpoint',
  )
  .option('--registry <host>', 'registry to push the echo-server image to (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await envVarsScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('api-key-scenario')
  .description(
    'Real API-key lifecycle via HTTP: a second low-privilege user creates a scoped API key through POST /api-keys ' +
      '(session-cookie authenticated, since machine principals cannot create keys), the key gets 200 on an ' +
      'in-scope request and 403 on an out-of-scope one, then revocation makes an immediate retry fail -- ' +
      'not just that api-key CRUD returns 2xx',
  )
  .option('--temps-root <path>', 'checkout root (needs crates/temps-cli) to mint the second user\'s bearer key via DB-direct `temps api-key`; or $TEMPS_ROOT')
  .option('--database-url <url>', 'Postgres URL the temps binary can reach directly; or $TEMPS_DATABASE_URL')
  .option('--keep', 'do not tear down created resources')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await apiKeyScenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('multinode-join-scenario')
  .description(
    'Real direct-underlay multi-node/mTLS proof: brings up its OWN dedicated 2-node DinD cluster ' +
      '(tools/e2e-multinode-cluster/, not the shared instance -- no --url/--api-key), waits for a real ' +
      'worker to register via POST /internal/nodes/register, pins a deployment to it via target_nodes, ' +
      'proves the container actually landed on the worker (not the control plane) via a docker-exec side ' +
      'channel, proves app-to-app and managed-Postgres *.temps.local DNS from deployed containers, drains ' +
      'the worker, and removes it from the cluster -- the first e2e coverage this feature ' +
      'has ever had. First run compiles the temps binary from source TWICE (once per node) inside Docker, ' +
      'so budget 15-20+ minutes; subsequent runs are fast (cargo/target caches persist across runs).',
  )
  .option('--compose-file <path>', 'path to the cluster docker-compose.yml', undefined)
  .option('--build-timeout <ms>', 'max wait for the cluster/worker to come up (real cargo build inside Docker)', '1800000')
  .option('--keep', 'do not tear down the cluster (leaves the entire 2-node cluster running, not just one container)')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await multinodeJoinScenarioCommand(opts)
  })

program
  .command('markdown')
  .description(
    'Deploy a real app and verify Accept: text/markdown content negotiation (Markdown for Agents) through the live proxy',
  )
  .option(
    '--image <ref>',
    'public Docker image to deploy (must run as non-root; see README)',
    'nginxinc/nginx-unprivileged:alpine',
  )
  .option('--port <port>', 'container port the image listens on', '80')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await markdownCommand({ ...opts, connection: connection() })
  })

program.parseAsync().catch((err: unknown) => {
  process.stderr.write(`\nerror: ${(err as Error).message}\n`)
  process.exit(1)
})
