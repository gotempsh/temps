/**
 * S3/MinIO managed-service restore scenario — proves the newly added
 * `restore_in_place` capability is genuinely functional, not just a 2xx.
 *
 * Flow:
 *   1. Provision a real S3/MinIO managed service with KNOWN credentials so the
 *      test can interact with it directly via Bun.S3Client.
 *   2. Create two buckets in the live service via a one-shot `mc mb` container
 *      (same approach restore itself uses) and upload 3 recognisable objects:
 *      `alpha/file-a.txt`, `alpha/file-b.txt`, `beta/file-c.txt`.
 *   3. Create an S3 source pointing at the local test MinIO (the backup
 *      destination, same endpoint backup-restore-scenario uses) and trigger a
 *      real backup.  This uses the `S3MirrorEngine` (`mc mirror source/ dest/`)
 *      path — a genuine mc mirror, not a stub.
 *   4. After the backup completes:
 *        a. Upload a POST-backup object: `alpha/post-backup.txt`
 *        b. Delete a PRE-backup object: `alpha/file-b.txt`
 *      This creates a state divergence between the live service and the backup.
 *   5. Trigger restore_in_place via `POST /external-services/{id}/restore`.
 *      The implementation uses `mc mirror --overwrite --remove`, so:
 *        — `alpha/file-b.txt` must come BACK (was deleted after backup)
 *        — `alpha/post-backup.txt` must DISAPPEAR (was added after backup)
 *        — `alpha/file-a.txt` and `beta/file-c.txt` must be present
 *   6. Assert via the platform read-only data-browser API
 *      (`GET /external-services/{id}/query/containers/{bucket}/entities`) that
 *      the live bucket matches the backup-time state exactly — not just that
 *      the restore run's status flipped to "completed".
 *   7. Teardown.
 *
 * Requires:
 *   — A running Temps instance (--url / TEMPS_URL)
 *   — A test MinIO for backup storage (--minio-endpoint, default localhost:9092)
 *     with the backup bucket already created (--minio-bucket, default temps-e2e-backups)
 *   — Docker available on the host (the test runs `docker run minio/mc mb` for
 *     bucket creation in the live service, which has no platform write API)
 *
 * ⚠ restore_in_place uses `--remove`: this DELETES live objects absent from the
 * backup.  Do NOT run this against a production S3 service.
 *
 * Bug fixed while building this scenario (root cause, not workaround):
 *   `restore_from_s3` (the legacy CLI path) used `--skip-errors` which silently
 *   swallowed mc mirror failures, and did not check mirror exit codes.  The new
 *   `restore_in_place` override replaces the interactive-exec container pattern
 *   with a proper one-shot container, drops `--skip-errors`, and propagates
 *   non-zero exit codes as real errors.
 */
import {
  createS3Source,
  deleteS3Source,
  runExternalServiceBackup,
  listExternalServiceBackups,
  getBackup,
  startRestore,
  getRestoreRun,
  listEntities,
  getService,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eService, pollUntil, teardown, makeRunId } from '../lib/flows.ts'

export interface S3RestoreScenarioOptions {
  minioEndpoint?: string
  minioBucket?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface S3RestoreScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

// ── Bucket operations via one-shot mc container ───────────────────────────────

/** Run a one-shot `docker run --rm minio/mc …` command and return its exit code + output. */
async function runMc(args: string[], envVars: string[]): Promise<{ code: number; output: string }> {
  const dockerArgs = [
    'run',
    '--rm',
    '--network',
    'host',
    ...envVars.flatMap((e) => ['-e', e]),
    'minio/mc:latest',
    ...args,
  ]
  const proc = Bun.spawn(['docker', ...dockerArgs], { stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { code, output: (stdout + stderr).trim() }
}

/** Create a bucket in the live MinIO service (mc mb). Ignores "already exists". */
async function createBucket(alias: string, bucket: string, envVars: string[]): Promise<void> {
  const { code, output } = await runMc(['mb', `${alias}/${bucket}`], envVars)
  if (code !== 0 && !output.includes('already') && !output.includes('exists')) {
    throw new Error(`mc mb ${alias}/${bucket} failed (exit ${code}): ${output}`)
  }
}

// ── Bun.S3Client helpers ──────────────────────────────────────────────────────

function makeLiveS3Client(
  port: string,
  accessKey: string,
  secretKey: string,
): InstanceType<typeof Bun.S3Client> {
  return new Bun.S3Client({
    endpoint: `http://localhost:${port}`,
    region: 'us-east-1',
    accessKeyId: accessKey,
    secretAccessKey: secretKey,
  })
}

async function putObject(
  s3: InstanceType<typeof Bun.S3Client>,
  bucket: string,
  key: string,
  content: string,
): Promise<void> {
  await s3.write(`${bucket}/${key}`, content, { type: 'text/plain' })
}

async function deleteObject(
  s3: InstanceType<typeof Bun.S3Client>,
  bucket: string,
  key: string,
): Promise<void> {
  await s3.delete(`${bucket}/${key}`)
}

// ── Scenario entry point ──────────────────────────────────────────────────────

export async function s3RestoreScenarioCommand(opts: S3RestoreScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps S3 restore scenario  ->  ${cfg.url}`)

  // The backup destination: a test MinIO that already exists and has the bucket.
  const backupMinioEndpoint = opts.minioEndpoint ?? 'http://localhost:9092'
  const backupMinioBucket = opts.minioBucket ?? 'temps-e2e-backups'

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

  // Fixed known credentials so we can talk to the live service directly.
  const LIVE_ACCESS_KEY = 'e2erestorekey01'
  const LIVE_SECRET_KEY = 'e2erestoresecret0123456'

  const serviceIds: number[] = []
  let s3SourceId: number | undefined
  let livePort: string | undefined
  let s3: InstanceType<typeof Bun.S3Client> | undefined

  try {
    // ── 1. Provision the live S3/MinIO service ────────────────────────────────
    const service = await step('provision live S3/MinIO service with known credentials', () =>
      createE2eService(client, {
        name: `${runId}-s3`,
        serviceType: 's3',
        parameters: {
          access_key: LIVE_ACCESS_KEY,
          secret_key: LIVE_SECRET_KEY,
        },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    // ── 2. Fetch the port the container bound to ──────────────────────────────
    livePort = await step('read live MinIO port from service parameters', async () => {
      const details = unwrap(
        await getService({ client, path: { id: service.id } }),
        'getService',
      ) as { current_parameters?: { [k: string]: string } | null }
      const port = details.current_parameters?.port
      if (!port) throw new Error('port not found in service parameters')
      log(`  port=${port}`)
      return port
    })

    s3 = makeLiveS3Client(livePort, LIVE_ACCESS_KEY, LIVE_SECRET_KEY)

    // Env vars for the one-shot mc container that creates buckets.
    const liveMcEnv = [
      `MC_HOST_live=http://${LIVE_ACCESS_KEY}:${LIVE_SECRET_KEY}@localhost:${livePort}`,
    ]

    // ── 3. Create buckets + upload pre-backup objects ─────────────────────────
    await step('create bucket alpha and upload pre-backup objects', async () => {
      await createBucket('live', 'alpha', liveMcEnv)
      await putObject(s3!, 'alpha', 'file-a.txt', `content-a-${runId}`)
      await putObject(s3!, 'alpha', 'file-b.txt', `content-b-${runId}`)
      log('  uploaded alpha/file-a.txt, alpha/file-b.txt')
    })

    await step('create bucket beta and upload pre-backup object', async () => {
      await createBucket('live', 'beta', liveMcEnv)
      await putObject(s3!, 'beta', 'file-c.txt', `content-c-${runId}`)
      log('  uploaded beta/file-c.txt')
    })

    // ── 4. Create S3 source + trigger real backup ─────────────────────────────
    const source = await step('create S3 source for backup destination (test MinIO)', async () => {
      const res = unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-bkp`,
            bucket_name: backupMinioBucket,
            bucket_path: runId,
            access_key_id: 'minioadmin',
            secret_key: 'minioadmin',
            region: 'us-east-1',
            endpoint: backupMinioEndpoint,
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

    await step('trigger real S3 mirror backup (mc mirror source/ dest/)', async () => {
      unwrap(
        await runExternalServiceBackup({
          client,
          path: { id: service.id },
          body: { s3_source_id: source.id, backup_type: 'full' },
        }),
        'runExternalServiceBackup',
      )
    })

    const backupEntry = await step('poll backup to completed state', async () => {
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
        { timeoutMs: 60_000, intervalMs: 2_000, label: 'backup row to appear' },
      )
      const entry = list.backups[0]!
      const final = await pollUntil(
        async () => unwrap(await getBackup({ client, path: { id: entry.backup_id } }), 'getBackup'),
        (b) => b.state === 'completed' || b.state === 'failed',
        {
          timeoutMs: 180_000,
          intervalMs: 3_000,
          onPoll: (b) => log(`    ...${b.state}`),
          label: 'backup to reach terminal state',
        },
      )
      if (final.state !== 'completed') {
        throw new Error(`backup ${entry.backup_id} ended in state "${final.state}": ${final.error_message}`)
      }
      log(`  backup #${entry.id} (${entry.backup_id}) completed`)
      return entry
    })

    // ── 5. Diverge: post-backup add + delete ──────────────────────────────────
    await step('add post-backup object (alpha/post-backup.txt)', async () => {
      await putObject(s3!, 'alpha', 'post-backup.txt', `post-backup-content-${runId}`)
      log('  uploaded alpha/post-backup.txt — this object must disappear after restore')
    })

    await step('delete pre-backup object (alpha/file-b.txt)', async () => {
      await deleteObject(s3!, 'alpha', 'file-b.txt')
      log('  deleted alpha/file-b.txt — this object must come back after restore')
    })

    // Verify divergence before restore (belt-and-suspenders).
    await step('confirm live state is diverged before restore', async () => {
      const entities = unwrap(
        await listEntities({
          client,
          path: { service_id: service.id, path: 'alpha' },
        }),
        'listEntities (pre-restore)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name)
      log(`  pre-restore alpha objects: ${keys.join(', ')}`)
      if (!keys.includes('post-backup.txt')) {
        throw new Error('post-backup.txt should be visible before restore (confirms it was uploaded)')
      }
      if (keys.includes('file-b.txt')) {
        throw new Error('file-b.txt should be ABSENT before restore (confirms it was deleted)')
      }
    })

    // ── 6. Restore in-place ───────────────────────────────────────────────────
    const restoreRun = await step('start restore_in_place (--overwrite --remove)', async () => {
      const run = unwrap(
        await startRestore({
          client,
          path: { id: service.id },
          body: { backup_id: backupEntry.id, mode: 'in_place' },
        }),
        'startRestore',
      )
      log(`  restore run #${run.id}`)
      return run
    })

    await step('poll restore run to completed state', async () => {
      const final = await pollUntil(
        async () =>
          unwrap(await getRestoreRun({ client, path: { id: restoreRun.id } }), 'getRestoreRun'),
        (r) => r.status === 'completed' || r.status === 'failed',
        {
          timeoutMs: 180_000,
          intervalMs: 3_000,
          onPoll: (r) => log(`    ...${r.status} (${r.phase})`),
          label: 'restore run to reach terminal state',
        },
      )
      if (final.status !== 'completed') {
        throw new Error(
          `restore run ${restoreRun.id} ended in status "${final.status}": ${final.error_message}`,
        )
      }
    })

    // ── 7. Verify via data-browser API ────────────────────────────────────────
    await step('verify alpha: file-a.txt present (pre-backup, untouched)', async () => {
      const entities = unwrap(
        await listEntities({ client, path: { service_id: service.id, path: 'alpha' } }),
        'listEntities (post-restore alpha)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name)
      log(`  post-restore alpha objects: ${keys.join(', ')}`)
      if (!keys.includes('file-a.txt')) {
        throw new Error(
          `file-a.txt missing after restore — expected it to be present (was in backup). Keys: ${keys.join(', ')}`,
        )
      }
    })

    await step('verify alpha: file-b.txt restored (was deleted post-backup)', async () => {
      const entities = unwrap(
        await listEntities({ client, path: { service_id: service.id, path: 'alpha' } }),
        'listEntities (post-restore alpha file-b)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name)
      if (!keys.includes('file-b.txt')) {
        throw new Error(
          `file-b.txt was NOT restored — it was deleted after the backup and restore should have brought it back. Keys: ${keys.join(', ')}`,
        )
      }
      log('  file-b.txt is back — restore reverted the post-backup deletion')
    })

    await step('verify alpha: post-backup.txt absent (--remove deleted it)', async () => {
      const entities = unwrap(
        await listEntities({ client, path: { service_id: service.id, path: 'alpha' } }),
        'listEntities (post-restore alpha post-backup)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name)
      if (keys.includes('post-backup.txt')) {
        throw new Error(
          `post-backup.txt is still present after restore — restore_in_place with --remove should have deleted it. Keys: ${keys.join(', ')}`,
        )
      }
      log('  post-backup.txt is gone — --remove semantics confirmed')
    })

    await step('verify beta: file-c.txt present (pre-backup, second bucket)', async () => {
      const entities = unwrap(
        await listEntities({ client, path: { service_id: service.id, path: 'beta' } }),
        'listEntities (post-restore beta)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name)
      log(`  post-restore beta objects: ${keys.join(', ')}`)
      if (!keys.includes('file-c.txt')) {
        throw new Error(
          `file-c.txt missing from beta bucket after restore. Keys: ${keys.join(', ')}`,
        )
      }
    })

    await step('verify exact alpha object count (2: file-a + file-b only)', async () => {
      const entities = unwrap(
        await listEntities({ client, path: { service_id: service.id, path: 'alpha' } }),
        'listEntities (post-restore count)',
      ) as { entities: Array<{ name: string }> }
      const keys = entities.entities.map((e) => e.name).sort()
      if (keys.length !== 2 || !keys.includes('file-a.txt') || !keys.includes('file-b.txt')) {
        throw new Error(
          `alpha bucket has unexpected contents after restore. Expected [file-a.txt, file-b.txt], got [${keys.join(', ')}]`,
        )
      }
      log(`  alpha has exactly 2 objects: ${keys.join(', ')} ✓`)
    })
  } catch {
    // Failure already recorded in steps; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept: services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'})`,
      )
    } else {
      if (s3SourceId !== undefined) {
        await deleteS3Source({ client, path: { id: s3SourceId } }).catch(() => undefined)
      }
      const td = await teardown(client, { serviceIds })
      log(
        `\n▶ teardown: deleted ${td.deletedServices} service(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: S3RestoreScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ s3-restore-scenario PASSED' : '❌ s3-restore-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
