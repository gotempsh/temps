/**
 * PostgreSQL major-version upgrade (17 → 18) against a live Temps instance,
 * using MinIO as the local S3-compatible target for the mandatory pre-upgrade
 * backup (same infra as `backup-restore-scenario` and `pitr-scenario`):
 *
 *   1. provision a real standalone postgres service pinned explicitly to
 *      `gotempsh/postgres-walg:17-bookworm` (not the platform default which is 18).
 *      Published platform-reviewed images are used; the platform extracts
 *      "17" from the tag.
 *   2. create a **default** S3 source (`is_default: true`) pointed at the
 *      local MinIO. `phase_pre_backup` in the orchestrator resolves the
 *      default source via `BackupService::default_s3_source_id` (a simple
 *      `WHERE is_default = true` query -- see
 *      `crates/temps-backup/src/services/backup.rs`). The upgrade API
 *      returns 412 if no default source is configured.
 *   3. link the service to a project and deploy the `db-probe` Go app
 *      (same as `backup-restore-scenario` / `pitr-scenario`). The app boots,
 *      creates `e2e_probe`, and on every `/probe` hit does a real INSERT +
 *      SELECT count(*).
 *   4. write 5 marker rows through the probe (T1 marker set). These land in
 *      the per-project auto-provisioned database (NOT the service's own
 *      `database` parameter -- see `PostgresService::get_runtime_env_vars` /
 *      `normalizePostgresDatabaseName`'s doc comment in flows.ts).
 *   5. start the upgrade: `POST /external-services/{id}/upgrades` with
 *      from_version="17", to_version="18", with reviewed managed images.
 *      The orchestrator runs phases
 *      synchronously inside a spawned tokio task:
 *        pre_backup  → wal-g backup to MinIO (pre-upgrade safety net)
 *        snapshot    → stop old container, copy PGDATA volume to rollback vol
 *        dump        → pg_dumpall from old container into a dump volume
 *        new_container → create fresh PostgreSQL 18 container (initdb on empty vol)
 *        restore     → psql restore from dump into new container
 *        swap        → persist to_image onto service config; restart container
 *        analyze     → ANALYZE for planner stats
 *        completed   → terminal success
 *   6. poll `GET /external-services/{service_id}/upgrades/{id}` until
 *      `status === "completed"` or `status === "failed"`. The upgrade is
 *      real work (wal-g backup, pg_dumpall, psql restore): allow up to 10
 *      minutes. On timeout, include the last-seen phase and error_message in
 *      the failure so it's diagnostic rather than mysterious.
 *   7. assert the 5 T1 marker rows survived the dump → restore cycle. Read
 *      them back through the read-only data-browser API (`readEntityRows` --
 *      the same `GET /external-services/{id}/query/...` endpoint
 *      `pitr-scenario` uses), NOT via `/probe` which mutates on every call.
 *      This is the real proof: if pg_dumpall captured the rows and psql
 *      restored them, they show up here. A status flip alone doesn't prove
 *      data integrity.
 *   8. assert the service is writable on the new version: hit `/probe` once
 *      more and confirm a 6th row landed (5 restored + 1 new). The Postgres
 *      sequence jumps past the old high-water mark after a dump+restore for
 *      the same reason PITR does (WAL-logged `SEQ_LOG_VALS` advance), so we
 *      assert the count and that the new row's id is > 5, not that it equals 6.
 *   9. assert the service's reported `docker_image` parameter now reflects
 *      the new image (`gotempsh/postgres-walg:18-bookworm`). `phase_swap`
 *      calls `lifecycle.set_docker_image` which persists the new image into
 *      `external_services.parameters` -- `GET /external-services/{id}`'s
 *      `current_parameters.docker_image` is the API-visible proof.
 *  10. teardown (deployment, project, service, S3 source)
 *
 * Needs the local MinIO from `docker-compose.e2e.yml` (`minio` service) and
 * a bucket created ahead of time -- see "External-service test infra" in
 * apps/temps-e2e/README.md.
 */
import {
  linkServiceToProject,
  createS3Source,
  deleteS3Source,
  startPgUpgrade,
  getPgUpgrade,
  getService,
  readEntityRows,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  createE2eService,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  resolveLoadTarget,
  pollUntil,
  teardown,
  makeRunId,
  sleep,
  normalizePostgresDatabaseName,
} from '../lib/flows.ts'
import { buildProbeImage, PROBE_APP_HEALTH_PATH } from '../lib/probe-app.ts'

// The from/to images MUST be on the same OS family (Alpine ↔ Alpine or
// Debian ↔ Debian) -- `validate_os_family` in postgres_upgrade.rs enforces
// this at start time. Temps' reviewed managed images are used here; the platform's
// extract_postgres_version() parses the major version from the tag correctly,
// and the managed-service allowlist admits only platform-reviewed images.
const FROM_IMAGE = 'gotempsh/postgres-walg:17-bookworm'
const TO_IMAGE = 'gotempsh/postgres-walg:18-bookworm'
const FROM_VERSION = '17'
const TO_VERSION = '18'

const PROBE_TABLE_ENTITY = 'e2e_probe'

export interface PgUpgradeScenarioOptions {
  registry?: string
  minioEndpoint?: string
  minioBucket?: string
  keep?: boolean
  deployTimeout?: string
  upgradeTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface PgUpgradeScenarioResult {
  runId: string
  ok: boolean
  upgradeId?: number
  steps: StepLog[]
}

async function hitProbe(url: string, headers: Record<string, string>): Promise<number> {
  const res = await fetch(`${url.replace(/\/+$/, '')}/probe`, { headers })
  if (res.status !== 200) throw new Error(`GET /probe returned HTTP ${res.status}`)
  const body = (await res.json()) as { count: number }
  return body.count
}

/** Read every `e2e_probe` row's `id` back through the real data-browser API, sorted ascending. */
async function readProbeIds(
  client: Parameters<typeof readEntityRows>[0]['client'],
  serviceId: number,
  dbContainerPath: string,
): Promise<{ totalCount: number; ids: number[] }> {
  const result = unwrap(
    await readEntityRows({
      client,
      path: { service_id: serviceId, path: dbContainerPath, entity: PROBE_TABLE_ENTITY },
      query: { limit: 50, sort_by: 'id', sort_order: 'asc' },
    }),
    'readEntityRows',
  )
  const ids = (result.rows as Array<Record<string, unknown>>).map((r) => Number(r.id))
  return { totalCount: result.total_count, ids }
}

export async function pgUpgradeScenarioCommand(opts: PgUpgradeScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps pg-upgrade-scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the probe app image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY.',
    )
  }
  // `temps serve` runs natively on the host and does its own S3 operations
  // from that same process, so `localhost` is correct here.
  const minioEndpoint = opts.minioEndpoint ?? 'http://localhost:9092'
  const minioBucket = opts.minioBucket ?? 'temps-e2e-backups'

  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')
  const upgradeTimeoutMs = Number(opts.upgradeTimeout ?? '600000')
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-probe`

  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let s3SourceId: number | undefined
  let upgradeId: number | undefined

  try {
    const imageRef = await step('build + push db-probe image', () =>
      buildProbeImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )

    // Provision a postgres service explicitly pinned to the older major version.
    // The `docker_image` parameter overrides the platform default so the service
    // actually runs Postgres 16. Both reviewed images use the same Debian
    // family, as required by `validate_os_family` in postgres_upgrade.rs.
    const service = await step(`provision postgres service pinned to ${FROM_IMAGE}`, () =>
      createE2eService(client, {
        name: `${runId}-pg`,
        serviceType: 'postgres',
        parameters: {
          database: 'app',
          username: 'app',
          docker_image: FROM_IMAGE,
        },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-pg-upgrade`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('link service to project', async () => {
      unwrap(
        await linkServiceToProject({ client, path: { id: service.id }, body: { project_id: project.id } }),
        'linkServiceToProject',
      )
    })

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.slug})`)

    // The db container path for the data-browser API is the auto-provisioned
    // per-project-per-environment database, NOT the service's own `database`
    // parameter. `PostgresService::get_runtime_env_vars` names it
    // normalize_database_name("{project_slug}_{env_slug}") -- see
    // `pitr-scenario.ts` for the same pattern.
    const dbContainerPath = `${normalizePostgresDatabaseName(`${project.slug}_${env.slug}`)}/public`
    log(`  db container path: ${dbContainerPath}`)

    const deploymentId = await step('deploy the db-probe app', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef }),
    )
    deployments.push({ projectId: project.id, deploymentId })

    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) throw new Error(`deployment ${deploymentId} is in state "${s.state}"`)
    })

    const target = resolveLoadTarget(cfg.url, env.mainUrl)
    await step('wait for the app to serve real traffic', async () => {
      await waitForHttpReady({ url: target.url, headers: target.headers })
      await assertNotConsoleFallback({ url: target.url, headers: target.headers })
    })

    await step('health check reports a real DB ping succeeding', async () => {
      const res = await fetch(`${target.url.replace(/\/+$/, '')}${PROBE_APP_HEALTH_PATH}`, {
        headers: target.headers,
      })
      if (res.status !== 200) throw new Error(`GET ${PROBE_APP_HEALTH_PATH} returned HTTP ${res.status}`)
    })

    // Create a **default** S3 source -- required for the upgrade's pre_backup
    // phase. `phase_pre_backup` resolves the default source via
    // `BackupService::default_s3_source_id` (`WHERE is_default = true`). The
    // upgrade API returns 412 "No default S3 source configured" without this.
    // Note: if an existing default S3 source is already configured on this
    // instance, setting is_default=true here will demote the old one to
    // is_default=false in the same DB transaction (the platform enforces
    // at-most-one default). Teardown deletes this source, leaving the instance
    // with no default (acceptable for a test-only instance; prod would have one
    // already).
    const source = await step('create a default S3 source pointed at the local MinIO', async () => {
      const res = unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-minio-default`,
            bucket_name: minioBucket,
            bucket_path: `${runId}-upgrade`,
            access_key_id: 'minioadmin',
            secret_key: 'minioadmin',
            region: 'us-east-1',
            endpoint: minioEndpoint,
            force_path_style: true,
            is_default: true,
          },
        }),
        'createS3Source',
      )
      return res
    })
    s3SourceId = source.id
    log(`  s3 source #${source.id} (is_default=true)`)

    await step('write 5 marker rows (T1) through the injected POSTGRES_URL', async () => {
      let last = 0
      for (let i = 0; i < 5; i++) {
        last = await hitProbe(target.url, target.headers)
      }
      if (last !== 5) throw new Error(`after 5x /probe, count=${last}, expected 5`)
    })

    const upgrade = await step(
      `start Postgres major-version upgrade ${FROM_VERSION} → ${TO_VERSION}`,
      async () => {
        const res = unwrap(
          await startPgUpgrade({
            client,
            path: { service_id: service.id },
            body: {
              from_version: FROM_VERSION,
              to_version: TO_VERSION,
              from_image: FROM_IMAGE,
              to_image: TO_IMAGE,
            },
          }),
          'startPgUpgrade',
        )
        return res
      },
    )
    upgradeId = upgrade.id
    log(`  upgrade #${upgrade.id} (log_id=${upgrade.log_id})`)
    log(`  initial status: ${upgrade.status}, phase: ${upgrade.phase}`)

    await step(
      `poll upgrade #${upgrade.id} through all phases to completed (timeout ${upgradeTimeoutMs / 1000}s)`,
      async () => {
        const finalUpgrade = await pollUntil(
          async () =>
            unwrap(
              await getPgUpgrade({
                client,
                path: { service_id: service.id, id: upgrade.id },
              }),
              'getPgUpgrade',
            ),
          (u) => u.status === 'completed' || u.status === 'failed' || u.status === 'cancelled',
          {
            timeoutMs: upgradeTimeoutMs,
            intervalMs: 5000,
            onPoll: (u) => log(`    ...status=${u.status} phase=${u.phase}`),
            label: 'upgrade to reach a terminal state',
          },
        )
        if (finalUpgrade.status !== 'completed') {
          throw new Error(
            `upgrade #${upgrade.id} ended in status "${finalUpgrade.status}" at phase "${finalUpgrade.phase}": ` +
              (finalUpgrade.error_message ?? '(no error message)'),
          )
        }
        log(`  upgrade completed at phase=${finalUpgrade.phase}`)
      },
    )

    // Wait briefly for the new container to fully accept connections before
    // hitting the data-browser API and /probe. The swap phase starts the new
    // container and waits for pg_isready, but the app may need a moment to
    // reconnect its connection pool.
    await step('short stabilization wait after upgrade completes', async () => {
      await sleep(3_000)
    })

    await step(
      'the 5 T1 marker rows survived the dump → restore cycle (read via data-browser API, not /probe)',
      async () => {
        const { totalCount, ids } = await readProbeIds(client, service.id, dbContainerPath)
        if (totalCount !== 5) {
          throw new Error(
            `after upgrade, e2e_probe has ${totalCount} row(s), expected exactly 5 -- ` +
              (totalCount === 0
                ? 'data was NOT restored into the new container (pg_dumpall/psql path failed silently)'
                : `unexpected data state: found ${totalCount} rows`),
          )
        }
        const expected = [1, 2, 3, 4, 5]
        if (JSON.stringify(ids) !== JSON.stringify(expected)) {
          throw new Error(
            `after upgrade, e2e_probe ids are ${JSON.stringify(ids)}, expected exactly ${JSON.stringify(expected)}`,
          )
        }
        log(`  5 marker rows confirmed: ids=${JSON.stringify(ids)}`)
      },
    )

    await step(
      'the upgraded service is fully writable -- a new write lands on top of the restored 5 rows',
      async () => {
        const count = await hitProbe(target.url, target.headers)
        // After a dump+restore, Postgres sequences jump past their pre-dump
        // high-water mark for the same reason PITR does: WAL-logged
        // `SEQ_LOG_VALS` advance, so the first nextval() after restore
        // legitimately jumps past the last issued value. We assert count
        // (not id == 6) and that the new row id is strictly > 5.
        const { totalCount, ids } = await readProbeIds(client, service.id, dbContainerPath)
        const restoredIds = ids.slice(0, 5)
        const newId = ids[5]
        const expectedRestored = [1, 2, 3, 4, 5]

        if (
          count !== 6 ||
          totalCount !== 6 ||
          ids.length !== 6 ||
          JSON.stringify(restoredIds) !== JSON.stringify(expectedRestored) ||
          newId === undefined ||
          newId <= 5
        ) {
          throw new Error(
            `after post-upgrade write: /probe count=${count}, data-browser total_count=${totalCount}, ` +
              `ids=${JSON.stringify(ids)} -- expected count=6, restored ids exactly ${JSON.stringify(expectedRestored)}, new id > 5`,
          )
        }
        log(`  new row landed as id ${newId} (post-restore sequence jump is expected)`)
      },
    )

    await step(
      `service config now reflects the new image (${TO_IMAGE}) -- phase_swap persisted it`,
      async () => {
        const svcDetails = unwrap(
          await getService({ client, path: { id: service.id } }),
          'getService',
        )
        // current_parameters is a string→string map. docker_image is the
        // field set by PostgresConfig / PostgresInputConfig.
        const dockerImage = svcDetails.current_parameters?.docker_image
        if (!dockerImage) {
          throw new Error(
            `current_parameters.docker_image is absent from GET /external-services/${service.id} -- ` +
              'phase_swap must persist the new image via lifecycle.set_docker_image; ' +
              'a missing field means the assertion cannot be made and the upgrade is unverified',
          )
        }
        if (dockerImage !== TO_IMAGE) {
          throw new Error(
            `after upgrade, service reports docker_image="${dockerImage}", expected "${TO_IMAGE}" -- ` +
              'phase_swap should have persisted the new image via lifecycle.set_docker_image',
          )
        }
        log(`  docker_image confirmed: ${dockerImage}`)
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: projects=${projectIds.join(',')} services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'} upgrade=${upgradeId ?? '-'})`,
      )
    } else {
      if (s3SourceId) {
        await deleteS3Source({ client, path: { id: s3SourceId } }).catch(() => undefined)
      }
      const td = await teardown(client, { deployments, projectIds, serviceIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s), ${td.deletedServices} service(s), S3 source` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: PgUpgradeScenarioResult = { runId, ok, upgradeId, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ pg-upgrade-scenario PASSED' : '❌ pg-upgrade-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
