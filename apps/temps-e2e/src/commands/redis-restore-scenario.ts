/**
 * Backup + restore lifecycle for a Redis managed service, using MinIO as the
 * local S3-compatible target (same image + credentials as the postgres
 * backup-restore and PITR scenarios):
 *
 *   1. provision a real standalone Redis service and link it to a project
 *   2. deploy a small Go HTTP probe app that connects via the injected
 *      `REDIS_URL` (includes the per-project database number) and exposes
 *      write endpoints for string, hash, and list keys
 *   3. create an S3 source pointed at the local MinIO
 *   4. write three PRE-BACKUP keys of distinct types (string, hash, list)
 *      through the probe app's HTTP endpoints
 *   5. trigger a real backup (`POST /backups/external-services/{id}/run`) and
 *      poll `GET /backups/{id}` to a real `completed` state (a genuine
 *      `wal-g backup-fetch` uploaded to S3 inside a WAL-G helper container)
 *   6. write three POST-BACKUP keys that must NOT survive the restore
 *   7. start an in-place restore from the backup (`POST
 *      /external-services/{id}/restore`, `mode: "in_place"`) and poll
 *      `GET /restore-runs/{id}` to `completed`
 *   8. verify via the read-only data-browser API (`readEntityRows`, the same
 *      endpoint `temps data rows` uses) that the three PRE-BACKUP keys still
 *      exist (HTTP 200) and the three POST-BACKUP keys are absent (HTTP 404)
 *      -- proving the restore actually reverted the post-backup writes, not
 *      just that the API calls returned 2xx
 *   9. teardown (deployment, project, service, S3 source)
 *
 * Redis restore mechanics (documented here because they're non-obvious):
 *   - The WAL-G helper container mounts the Redis container's volume via
 *     `volumes_from` while the Redis container is stopped.
 *   - WAL-G writes a `dump.rdb` into `/data`, then the helper reconstructs
 *     the Redis 7 multi-part AOF directory (`appendonlydir/`) with a manifest
 *     pointing to the RDB as the base file. This is the only correct path:
 *     Redis 7+ with `appendonly yes` ignores a bare `dump.rdb` on startup and
 *     reads from `appendonlydir/` instead.
 *   - The Redis container's restart policy is set to `no` before stopping it
 *     (otherwise Docker immediately restarts it, and the helper cannot write
 *     to the volume). It is always reset to `always` after the restore,
 *     regardless of success or failure.
 *
 * Needs the local MinIO from `docker-compose.e2e.yml` (`minio` service) and
 * a bucket created ahead of time -- see "External-service test infra" in
 * apps/temps-e2e/README.md.
 */
import {
  linkServiceToProject,
  createS3Source,
  deleteS3Source,
  runExternalServiceBackup,
  listExternalServiceBackups,
  getBackup,
  startRestore,
  getRestoreRun,
  readEntityRows,
  getResolvedEnvironmentVariableValue,
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
} from '../lib/flows.ts'
import { buildRedisProbeImage, REDIS_PROBE_HEALTH_PATH } from '../lib/redis-probe-app.ts'

export interface RedisRestoreScenarioOptions {
  registry?: string
  minioEndpoint?: string
  minioBucket?: string
  keep?: boolean
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface RedisRestoreScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/**
 * Call a probe write endpoint and assert HTTP 200.
 * `path` is e.g. `/write?key=foo&value=bar`.
 */
async function callProbe(
  baseUrl: string,
  headers: Record<string, string>,
  path: string,
): Promise<void> {
  const url = `${baseUrl.replace(/\/+$/, '')}${path}`
  const res = await fetch(url, { headers })
  if (res.status !== 200) {
    const body = await res.text().catch(() => '')
    throw new Error(`GET ${path} returned HTTP ${res.status}: ${body}`)
  }
}

/**
 * Assert that a Redis key exists in the given database via the data-browser
 * API (HTTP 200 with non-empty rows). Throws if the key is absent (404) or
 * the request otherwise fails.
 */
async function assertKeyExists(
  client: Parameters<typeof readEntityRows>[0]['client'],
  serviceId: number,
  dbNumber: string,
  keyName: string,
): Promise<void> {
  const result = await readEntityRows({
    client,
    path: { service_id: serviceId, path: dbNumber, entity: keyName },
    query: { limit: 1 },
  })
  if (result.response?.status === 404) {
    throw new Error(
      `key "${keyName}" not found in Redis DB ${dbNumber} after restore (expected to survive)`,
    )
  }
  if (result.error !== undefined && result.error !== null) {
    const detail =
      typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error)
    throw new Error(
      `readEntityRows for key "${keyName}" failed (HTTP ${result.response?.status}): ${detail}`,
    )
  }
  if (!result.data) {
    throw new Error(`readEntityRows for key "${keyName}": empty response`)
  }
}

/**
 * Assert that a Redis key is absent in the given database (HTTP 404). Throws
 * if the key still exists or the request fails for an unexpected reason.
 */
async function assertKeyAbsent(
  client: Parameters<typeof readEntityRows>[0]['client'],
  serviceId: number,
  dbNumber: string,
  keyName: string,
): Promise<void> {
  const result = await readEntityRows({
    client,
    path: { service_id: serviceId, path: dbNumber, entity: keyName },
    query: { limit: 1 },
  })
  if (result.response?.status === 404) {
    // Correct: key does not exist after restore.
    return
  }
  if (result.data) {
    throw new Error(
      `key "${keyName}" still present in Redis DB ${dbNumber} after restore (should have been erased)`,
    )
  }
  // Any unexpected error other than 404.
  const detail =
    typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error)
  throw new Error(
    `readEntityRows for key "${keyName}" failed unexpectedly (HTTP ${result.response?.status}): ${detail}`,
  )
}

export async function redisRestoreScenarioCommand(
  opts: RedisRestoreScenarioOptions,
): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps redis-restore scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the redis-probe image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY.',
    )
  }
  // `temps serve` runs natively on the host. S3 uploads and WAL-G helper
  // containers both reach MinIO as `localhost` from the host's network
  // namespace, not `host.docker.internal` (which only resolves from inside a
  // container).
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
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-redis-probe`

  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let s3SourceId: number | undefined

  // Pre-backup keys (must survive restore).
  const PRE_STRING_KEY = `${runId}-pre-string`
  const PRE_HASH_KEY = `${runId}-pre-hash`
  const PRE_LIST_KEY = `${runId}-pre-list`
  // Post-backup keys (must be erased by restore).
  const POST_STRING_KEY = `${runId}-post-string`
  const POST_HASH_KEY = `${runId}-post-hash`
  const POST_LIST_KEY = `${runId}-post-list`

  try {
    const imageRef = await step('build + push redis-probe image', () =>
      buildRedisProbeImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )

    const service = await step('provision a real standalone Redis service', () =>
      createE2eService(client, {
        name: `${runId}-redis`,
        serviceType: 'redis',
        parameters: {},
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-redis-restore-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('link Redis service to project', async () => {
      unwrap(
        await linkServiceToProject({ client, path: { id: service.id }, body: { project_id: project.id } }),
        'linkServiceToProject',
      )
    })

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))

    const deploymentId = await step('deploy the redis-probe app', () =>
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

    await step('health check reports a real Redis PING succeeding', async () => {
      const res = await fetch(`${target.url.replace(/\/+$/, '')}${REDIS_PROBE_HEALTH_PATH}`, {
        headers: target.headers,
      })
      if (res.status !== 200) throw new Error(`GET ${REDIS_PROBE_HEALTH_PATH} returned HTTP ${res.status}`)
    })

    // Discover the Redis database number injected by the platform. This is
    // computed from a hash of the project+environment slug; the only reliable
    // way to retrieve it without replicating the hash in TypeScript is to read
    // it back from the resolved env vars.
    const dbNumber = await step('resolve REDIS_DATABASE from resolved env vars', async () => {
      const res = unwrap(
        await getResolvedEnvironmentVariableValue({
          client,
          path: { project_id: project.id, key: 'REDIS_DATABASE' },
          query: { environment_id: env.id },
        }),
        'getResolvedEnvironmentVariableValue(REDIS_DATABASE)',
      )
      if (!res.value || res.value === '***') {
        throw new Error(`REDIS_DATABASE resolved to "${res.value}", expected a numeric string`)
      }
      return res.value
    })
    log(`  REDIS_DATABASE=${dbNumber}`)

    const source = await step('create an S3 source pointed at the local MinIO', async () => {
      const res = unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-minio`,
            bucket_name: minioBucket,
            bucket_path: runId,
            access_key_id: 'minioadmin',
            secret_key: 'minioadmin',
            region: 'us-east-1',
            endpoint: minioEndpoint,
            force_path_style: true,
            is_default: false,
          },
        }),
        'createS3Source',
      )
      return res
    })
    s3SourceId = source.id
    log(`  s3 source #${source.id}`)

    await step('write 3 PRE-BACKUP keys (string, hash, list) via the probe app', async () => {
      await callProbe(
        target.url, target.headers,
        `/write?key=${encodeURIComponent(PRE_STRING_KEY)}&value=pre-string-value`,
      )
      await callProbe(
        target.url, target.headers,
        `/hset?key=${encodeURIComponent(PRE_HASH_KEY)}&field=f1&value=pre-hash-value`,
      )
      await callProbe(
        target.url, target.headers,
        `/lpush?key=${encodeURIComponent(PRE_LIST_KEY)}&value=pre-list-value`,
      )
    })

    await step('trigger a real Redis backup (wal-g backup-push -> upload to MinIO)', async () => {
      unwrap(
        await runExternalServiceBackup({
          client,
          path: { id: service.id },
          body: { s3_source_id: source.id, backup_type: 'full' },
        }),
        'runExternalServiceBackup',
      )
    })

    const backupEntry = await step('poll the backup to a real completed state', async () => {
      const list = await pollUntil(
        async () =>
          unwrap(
            await listExternalServiceBackups({
              client,
              path: { service_id: service.id },
              query: { page: 1, page_size: 5 },
            }),
            'listExternalServiceBackups',
          ),
        (l) => l.backups.length > 0,
        { timeoutMs: 30_000, intervalMs: 2000, label: 'backup row to appear' },
      )
      const entry = list.backups[0]!
      if (list.backups.length !== 1) {
        throw new Error(`expected exactly 1 backup for service ${service.id}, found ${list.backups.length}`)
      }

      const finalBackup = await pollUntil(
        async () => unwrap(await getBackup({ client, path: { id: entry.backup_id } }), 'getBackup'),
        (b) => b.state === 'completed' || b.state === 'failed',
        {
          timeoutMs: 180_000,
          intervalMs: 3000,
          onPoll: (b) => log(`    ...${b.state}`),
          label: 'backup to reach a terminal state',
        },
      )
      if (finalBackup.state !== 'completed') {
        throw new Error(
          `backup ${entry.backup_id} ended in state "${finalBackup.state}": ${finalBackup.error_message}`,
        )
      }
      return entry
    })
    log(`  backup #${backupEntry.id} (${backupEntry.backup_id})`)

    await step('write 3 POST-BACKUP keys that must NOT survive the restore', async () => {
      await callProbe(
        target.url, target.headers,
        `/write?key=${encodeURIComponent(POST_STRING_KEY)}&value=post-string-value`,
      )
      await callProbe(
        target.url, target.headers,
        `/hset?key=${encodeURIComponent(POST_HASH_KEY)}&field=f2&value=post-hash-value`,
      )
      await callProbe(
        target.url, target.headers,
        `/lpush?key=${encodeURIComponent(POST_LIST_KEY)}&value=post-list-value`,
      )
    })

    const restoreRun = await step('start an in-place restore from the pre-backup snapshot', async () => {
      const run = unwrap(
        await startRestore({
          client,
          path: { id: service.id },
          body: { backup_id: backupEntry.id, mode: 'in_place' },
        }),
        'startRestore',
      )
      return run
    })
    log(`  restore run #${restoreRun.id}`)

    await step('poll the restore to a real completed state', async () => {
      const finalRun = await pollUntil(
        async () => unwrap(await getRestoreRun({ client, path: { id: restoreRun.id } }), 'getRestoreRun'),
        (r) => r.status === 'completed' || r.status === 'failed',
        {
          timeoutMs: 180_000,
          intervalMs: 3000,
          onPoll: (r) => log(`    ...${r.status} (${r.phase})`),
          label: 'restore run to reach a terminal state',
        },
      )
      if (finalRun.status !== 'completed') {
        throw new Error(
          `restore run ${restoreRun.id} ended in status "${finalRun.status}": ${finalRun.error_message}`,
        )
      }
    })

    await step(
      'pre-backup keys survive: string key present in data-browser (HTTP 200)',
      () => assertKeyExists(client, service.id, dbNumber, PRE_STRING_KEY),
    )

    await step(
      'pre-backup keys survive: hash key present in data-browser (HTTP 200)',
      () => assertKeyExists(client, service.id, dbNumber, PRE_HASH_KEY),
    )

    await step(
      'pre-backup keys survive: list key present in data-browser (HTTP 200)',
      () => assertKeyExists(client, service.id, dbNumber, PRE_LIST_KEY),
    )

    await step(
      'post-backup keys erased: string key absent in data-browser (HTTP 404)',
      () => assertKeyAbsent(client, service.id, dbNumber, POST_STRING_KEY),
    )

    await step(
      'post-backup keys erased: hash key absent in data-browser (HTTP 404)',
      () => assertKeyAbsent(client, service.id, dbNumber, POST_HASH_KEY),
    )

    await step(
      'post-backup keys erased: list key absent in data-browser (HTTP 404)',
      () => assertKeyAbsent(client, service.id, dbNumber, POST_LIST_KEY),
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: projects=${projectIds.join(',')} services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'})`,
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
  const result: RedisRestoreScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ redis-restore-scenario PASSED' : '❌ redis-restore-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
