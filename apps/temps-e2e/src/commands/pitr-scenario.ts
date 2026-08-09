/**
 * Point-in-time recovery (PITR) against a live Temps instance, using MinIO
 * as the local S3-compatible target (same infra `backup-restore-scenario`
 * proves against):
 *   1. provision a real standalone postgres service, link it to a project,
 *      deploy the same `db-probe` app `backup-restore-scenario` uses
 *   2. create an S3 source pointed at the local MinIO
 *   3. trigger a real base backup (`POST /backups/external-services/{id}/run`)
 *      -- a real `wal-g backup-push` -- and poll to `completed`
 *   4. write 5 real rows through the injected `POSTGRES_URL` (the "T1"
 *      marker) and wait long enough for the WAL segment covering them to be
 *      archived to S3 (`archive_timeout=60s` on every managed postgres --
 *      see `PostgresService::create_container` -- forces a segment switch
 *      even though 5 tiny rows never fill a 16MB segment on their own; a
 *      restore's `wal-g wal-fetch` can only see WAL that's actually landed
 *      in S3, not whatever's still sitting in the live container's pg_wal)
 *   5. capture a recovery-target timestamp strictly AFTER T1 and strictly
 *      BEFORE the next write
 *   6. write 3 MORE rows (the "T2" marker, count now 8) -- data the PITR
 *      target must NOT include -- with a short wait afterward so T2's WAL
 *      is durably fsynced before we restore
 *   7. start a PITR restore in place to the captured T1 timestamp
 *      (`POST /external-services/{id}/restore`, `mode: "pitr"`) and poll
 *      `GET /restore-runs/{id}` to `completed`
 *   8. read the actual table back through the read-only data-browser API
 *      (`GET /external-services/{id}/query/containers/{path}/entities/{entity}/data`,
 *      the same endpoint `temps data rows` uses) -- an INDEPENDENT side
 *      channel from `/probe`, which mutates on every call -- and assert the
 *      exact row count (5) and exact primary-key set (`[1,2,3,4,5]`), not
 *      just that the restore run's status flipped to "completed"
 *   9. hit `/probe` once more and re-read the table, asserting total_count
 *      is 6, the first 5 ids are still exactly T1s `[1,2,3,4,5]`, and the
 *      new row's id is strictly greater than all of them -- proving the
 *      recovered cluster is fully writable on top of the restored data, not
 *      just readable
 *   10. teardown (deployment, project, service, S3 source)
 *
 * PITR is a real, reachable feature, not a stub: `RestoreRequestMode::Pitr`
 * (`crates/temps-backup/src/services/restore.rs`) requires a WAL-G backup
 * (`s3://` location) and dispatches to `PostgresService::restore_pitr`
 * (`crates/temps-providers/src/externalsvc/postgres.rs`), which writes a
 * real `recovery_target_time` line into `postgresql.auto.conf` and lets
 * Postgres itself replay WAL up to that point before promoting. This
 * scenario depends on the WAL-G continuous-archiving fix documented in
 * `backup-restore-scenario.ts` already being on `main` -- without it, no
 * backup on this platform is restorable at all, PITR included.
 *
 * Note (not a bug, standard Postgres behavior worth documenting because it
 * broke this scenario's first draft): the write in step 9 does NOT land as
 * id 6. Postgres WAL-logs sequence advances `SEQ_LOG_VALS` (32) values
 * ahead of what's actually been handed out, specifically so crash/PITR
 * recovery can never replay a value a client already received -- the first
 * `nextval()` after ANY recovery legitimately jumps past `count+1`. This
 * scenario asserts the row COUNT and the restored ids' exact identity
 * instead of the new row's specific id.
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

export interface PitrScenarioOptions {
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

interface PitrScenarioResult {
  runId: string
  ok: boolean
  pitrTargetTime?: string
  finalProbeCount?: number
  steps: StepLog[]
}

const PROBE_TABLE_ENTITY = 'e2e_probe'

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

export async function pitrScenarioCommand(opts: PitrScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps PITR scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the probe app image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY.',
    )
  }
  // Same reasoning as backup-restore-scenario: `temps serve` runs natively
  // on the host and does its own S3 upload/download from that same process,
  // so `localhost` is correct here, not `host.docker.internal`.
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
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-probe`

  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let s3SourceId: number | undefined
  let pitrTargetTime: string | undefined
  let finalProbeCount: number | undefined

  try {
    const imageRef = await step('build + push db-probe image', () =>
      buildProbeImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )

    const service = await step('provision a real standalone postgres service', () =>
      createE2eService(client, {
        name: `${runId}-pg`,
        serviceType: 'postgres',
        parameters: { database: 'app', username: 'app' },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-pitr-app`, exposedPort: 3000 }),
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

    // Linking a service does NOT hand the deployed app the database named
    // at service-creation time ("app") -- it auto-provisions a SEPARATE
    // per-project-per-environment database and injects `POSTGRES_URL`
    // pointed at THAT (see `PostgresService::get_runtime_env_vars` /
    // `normalizePostgresDatabaseName`'s doc comment). Reading the actual
    // table back through the data-browser API needs this computed name,
    // not the service's own `database` parameter. `get_runtime_env_vars`
    // keys off `environment.slug`, NOT the display `name` -- they're only
    // coincidentally identical for the default "production" environment.
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

    await step('trigger a real base backup (wal-g backup-push -> upload to MinIO)', async () => {
      unwrap(
        await runExternalServiceBackup({
          client,
          path: { id: service.id },
          body: { s3_source_id: source.id, backup_type: 'full' },
        }),
        'runExternalServiceBackup',
      )
    })

    const backupEntry = await step('poll the base backup to a real completed state', async () => {
      const list = await pollUntil(
        async () =>
          unwrap(
            await listExternalServiceBackups({ client, path: { service_id: service.id }, query: { page: 1, page_size: 5 } }),
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
        throw new Error(`backup ${entry.backup_id} ended in state "${finalBackup.state}": ${finalBackup.error_message}`)
      }
      return entry
    })
    log(`  backup #${backupEntry.id} (${backupEntry.backup_id})`)

    // Negative path: a recovery target from before this backup even started
    // is out of range -- no WAL this backup covers can ever satisfy it.
    // Root-cause note (why this is checked at the API layer, not just
    // asserted here): PostgreSQL itself does NOT error on a too-early
    // `recovery_target_time`. It silently stops recovery at the earliest
    // point it CAN reach (right after the base backup's own consistency
    // checkpoint) and carries on -- no FATAL, no warning. Verified live
    // against a real WAL-G backup before `RestoreService::start_restore`
    // gained this check: the API returned 202, the restore run reached
    // `status: "completed"`, and the "restored" table silently ended up
    // holding NONE of the data the backup covered -- a wrong state
    // reported as success, not a rejection. `start_restore` now range-checks
    // `RecoveryTarget::Time` against the backup's own `started_at` BEFORE a
    // restore run row is even created or any container is touched, so this
    // must come back as an immediate 400, not a run that completes into a
    // silently-wrong state.
    await step('PITR to a target before the backup started is rejected (400), not silently accepted', async () => {
      const outOfRangeTarget = '2000-01-01T00:00:00.000Z'
      const res = await startRestore({
        client,
        path: { id: service.id },
        body: {
          backup_id: backupEntry.id,
          mode: 'pitr',
          to_new_service: false,
          target: { kind: 'time', time: outOfRangeTarget },
        },
      })
      if (!res.error) {
        throw new Error(
          `PITR restore to ${outOfRangeTarget} (years before the backup started) succeeded ` +
            `(run #${res.data?.id}, status "${res.data?.status}") instead of being rejected -- ` +
            'an out-of-range PITR target must be REJECTED up front, not accepted into a restore ' +
            'run that can silently complete without honoring the requested target',
        )
      }
      const status = res.response?.status
      if (status !== 400) {
        throw new Error(`PITR restore to an out-of-range target returned HTTP ${status}, expected 400`)
      }
      const detail = (res.error as { detail?: string } | undefined)?.detail ?? ''
      if (!detail.toLowerCase().includes('before this backup started')) {
        throw new Error(`400 response did not explain the out-of-range target as expected; detail was: "${detail}"`)
      }
      log(`  rejected as expected: ${detail}`)
    })

    await step('write 5 real rows (T1 marker) through the injected POSTGRES_URL', async () => {
      let last = 0
      for (let i = 0; i < 5; i++) {
        last = await hitProbe(target.url, target.headers)
      }
      if (last !== 5) throw new Error(`after 5x /probe, count=${last}, expected 5 (T1)`)
    })

    // Every managed postgres runs with `archive_timeout=60` (see
    // `PostgresService::create_container`), which forces the Postgres
    // archiver to close and archive the current WAL segment every 60s even
    // if it's nowhere near the 16MB size threshold -- 5 tiny inserts never
    // trip that threshold on their own. A restore's `wal-g wal-fetch` can
    // only read WAL that's actually landed in S3 (`.ready`/archived), not
    // whatever's still sitting in the live container's pg_wal, so T1's
    // segment MUST have gone through at least one archive cycle before we
    // can PITR back to a timestamp inside it. 75s gives a 15s margin over
    // the 60s timeout for the archiver's own wakeup jitter.
    await step("wait for T1's WAL segment to be archived (archive_timeout=60s + margin)", async () => {
      await sleep(75_000)
    })

    pitrTargetTime = await step('capture the PITR recovery-target timestamp (after T1, before T2)', async () => {
      const iso = new Date().toISOString()
      log(`  target time: ${iso}`)
      return iso
    })

    await step('buffer so T2 is unambiguously after the captured target time', async () => {
      await sleep(3_000)
    })

    await step('write 3 MORE rows (T2 marker) the PITR target must NOT include (count now 8)', async () => {
      let last = 0
      for (let i = 0; i < 3; i++) {
        last = await hitProbe(target.url, target.headers)
      }
      if (last !== 8) throw new Error(`after 3 more /probe hits, count=${last}, expected 8`)
    })

    await step("short wait so T2's WAL is durably fsynced before we restore", async () => {
      await sleep(5_000)
    })

    const restoreRun = await step('start a PITR restore in place to the captured T1 timestamp', async () => {
      const run = unwrap(
        await startRestore({
          client,
          path: { id: service.id },
          body: {
            backup_id: backupEntry.id,
            mode: 'pitr',
            to_new_service: false,
            target: { kind: 'time', time: pitrTargetTime! },
          },
        }),
        'startRestore',
      )
      return run
    })
    log(`  restore run #${restoreRun.id}`)

    await step('poll the PITR restore to a real completed state', async () => {
      const finalRun = await pollUntil(
        async () => unwrap(await getRestoreRun({ client, path: { id: restoreRun.id } }), 'getRestoreRun'),
        (r) => r.status === 'completed' || r.status === 'failed',
        {
          timeoutMs: 240_000,
          intervalMs: 3000,
          onPoll: (r) => log(`    ...${r.status} (${r.phase})`),
          label: 'PITR restore run to reach a terminal state',
        },
      )
      if (finalRun.status !== 'completed') {
        throw new Error(`restore run ${restoreRun.id} ended in status "${finalRun.status}": ${finalRun.error_message}`)
      }
    })

    await step('the restored table has exactly T1s 5 rows (ids 1-5), T2 is gone -- read via the data-browser API, not /probe', async () => {
      const { totalCount, ids } = await readProbeIds(client, service.id, dbContainerPath)
      if (totalCount !== 5) {
        throw new Error(
          `after PITR restore, e2e_probe has ${totalCount} row(s), expected exactly 5 (T1) -- ` +
            `${totalCount === 8 ? 'the PITR restore did not revert the 3 post-target (T2) rows at all' : 'unexpected data state'}`,
        )
      }
      const expected = [1, 2, 3, 4, 5]
      if (JSON.stringify(ids) !== JSON.stringify(expected)) {
        throw new Error(`after PITR restore, e2e_probe ids are ${JSON.stringify(ids)}, expected exactly ${JSON.stringify(expected)}`)
      }
    })

    await step('the restored service is fully writable -- a new write lands on top of the restored 5, not colliding with them', async () => {
      const count = await hitProbe(target.url, target.headers)
      if (count !== 6) {
        throw new Error(`after PITR restore + 1 more /probe hit, count=${count}, expected 6 (5 restored + this insert)`)
      }
      const { totalCount, ids } = await readProbeIds(client, service.id, dbContainerPath)
      // NOT asserting the new row's id is exactly 6: Postgres sequences
      // WAL-log `SEQ_LOG_VALS` (32) values ahead of what's actually been
      // handed out, precisely so crash/PITR recovery can never replay a
      // value that was already returned to a client -- the first
      // `nextval()` after ANY recovery (crash or PITR) legitimately jumps
      // ahead of `count+1`. That's correct, documented Postgres behavior,
      // not a bug; asserting an exact id here would be asserting a false
      // invariant. What DOES have to hold: total_count is exactly 6, the
      // first 5 ids are still exactly T1s [1,2,3,4,5] (untouched by the new
      // write), and the new row's id is strictly greater than all of them
      // (landed on top, didn't collide with or overwrite restored data).
      const restoredIds = ids.slice(0, 5)
      const newId = ids[5]
      const expectedRestored = [1, 2, 3, 4, 5]
      if (
        totalCount !== 6 ||
        ids.length !== 6 ||
        JSON.stringify(restoredIds) !== JSON.stringify(expectedRestored) ||
        newId === undefined ||
        newId <= 5
      ) {
        throw new Error(
          `after the post-restore write, e2e_probe has total_count=${totalCount} ids=${JSON.stringify(ids)}, ` +
            `expected total_count=6, first 5 ids exactly ${JSON.stringify(expectedRestored)}, and a 6th id > 5`,
        )
      }
      log(`  new row landed as id ${newId}`)
      finalProbeCount = count
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'})`)
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
  const result: PitrScenarioResult = { runId, ok, pitrTargetTime, finalProbeCount, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ pitr-scenario PASSED' : '❌ pitr-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
