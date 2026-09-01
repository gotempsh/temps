// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Backup + restore lifecycle against a live Temps instance, using MinIO as
 * the local S3-compatible target (the same image + credentials already
 * proven in `crates/temps-backup`'s own testcontainers-based Rust tests):
 *   1. provision a real standalone postgres service, link it to a project,
 *      deploy the same `db-probe` app `managed-services-scenario` uses, and
 *      write 5 real rows through the real injected `POSTGRES_URL`
 *   2. create an S3 source pointed at the local MinIO
 *   3. trigger a real backup (`POST /backups/external-services/{id}/run`) --
 *      202 + async job, per ADR-014 -- and poll `GET /backups/{backup_id}`
 *      to a real `completed` state (a real `wal-g backup-push`, a real S3
 *      upload -- the postgres engine is WAL-G, not pg_dump)
 *   4. write 2 MORE rows (count now 7) -- data the backup does NOT contain
 *   5. start an in-place restore from the backup taken at step 3
 *      (`POST /external-services/{id}/restore`) and poll
 *      `GET /restore-runs/{id}` to `completed`
 *   6. hit `/probe` once more and assert the count is 6 (5 backed-up rows +
 *      this 1 new insert), NOT 8 -- proving the restore actually reverted
 *      the 2 post-backup rows, not just that the API calls returned 2xx
 *   7. teardown (deployment, project, service, S3 source)
 *
 * Needs the local MinIO from `docker-compose.e2e.yml` (`minio` service) and
 * a bucket created ahead of time -- see "External-service test infra" in
 * apps/temps-e2e/README.md.
 *
 * Bug found: `PostgresWalgEngine::run` (crates/temps-backup/src/engines/
 * postgres_walg.rs) ran `wal-g backup-push` without continuous WAL archiving
 * ever having been enabled on the target container, so the base backup's
 * checkpoint LSN had no archived WAL segment behind it -- every restore
 * failed at Postgres startup with "could not locate required checkpoint
 * record", the container crash-looped, and `wait_for_container_health`
 * eventually timed out at 90s. Moving continuous-archiving setup to run
 * *before* backup-push (not after -- `pg_stop_backup()` force-completes a
 * WAL segment that only gets a `.ready` marker if archiving is already on
 * at that moment) fixed it. Added `ExternalService::enable_continuous_archiving`
 * with a no-op default, implemented on `PostgresService` by reusing the
 * existing (private) `enable_wal_archiving`, and made it a no-op after the
 * first backup on a service so it doesn't recreate the container on every
 * single backup.
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
import { buildProbeImage, PROBE_APP_HEALTH_PATH } from '../lib/probe-app.ts'

export interface BackupRestoreScenarioOptions {
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

interface BackupRestoreScenarioResult {
  runId: string
  ok: boolean
  finalProbeCount?: number
  steps: StepLog[]
}

async function hitProbe(url: string, headers: Record<string, string>): Promise<number> {
  const res = await fetch(`${url.replace(/\/+$/, '')}/probe`, { headers })
  if (res.status !== 200) throw new Error(`GET /probe returned HTTP ${res.status}`)
  const body = (await res.json()) as { count: number }
  return body.count
}

export async function backupRestoreScenarioCommand(opts: BackupRestoreScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps backup-restore scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the probe app image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY.',
    )
  }
  // `temps serve` under test runs natively on the host (per the start-temps
  // skill), and `create_backup` does its own S3 upload from inside that same
  // process (the pg_dump/WAL-G work happens in a sidecar, but the dump is
  // piped back and uploaded by the host process's own aws-sdk-s3 client) --
  // so `localhost` is correct for both the synchronous connectivity check at
  // source-creation time AND the real backup upload. `host.docker.internal`
  // only resolves from inside a Docker container's network namespace, which
  // this process never runs in.
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
      createE2eProject(client, { name: `${runId}-backup-app`, exposedPort: 3000 }),
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

    await step('write 5 real rows through the injected POSTGRES_URL', async () => {
      let last = 0
      for (let i = 0; i < 5; i++) {
        last = await hitProbe(target.url, target.headers)
      }
      if (last !== 5) throw new Error(`after 5x /probe, count=${last}, expected 5`)
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

    await step('trigger a real backup (pg_dump -> upload to MinIO)', async () => {
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

    await step('write 2 MORE rows the backup does NOT contain (count now 7)', async () => {
      let last = 0
      for (let i = 0; i < 2; i++) {
        last = await hitProbe(target.url, target.headers)
      }
      if (last !== 7) throw new Error(`after 2 more /probe hits, count=${last}, expected 7`)
    })

    const restoreRun = await step('start an in-place restore from the count=5 backup', async () => {
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
        throw new Error(`restore run ${restoreRun.id} ended in status "${finalRun.status}": ${finalRun.error_message}`)
      }
    })

    await step('the restore reverted the 2 post-backup rows, not just returned 2xx', async () => {
      const count = await hitProbe(target.url, target.headers)
      if (count !== 6) {
        throw new Error(
          `after restore + 1 more /probe hit, count=${count}, expected 6 (5 backed-up rows + this insert) -- ` +
            `${count === 8 ? 'the restore did not revert the 2 post-backup rows at all' : 'unexpected data state'}`,
        )
      }
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
  const result: BackupRestoreScenarioResult = { runId, ok, finalProbeCount, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ backup-restore-scenario PASSED' : '❌ backup-restore-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
