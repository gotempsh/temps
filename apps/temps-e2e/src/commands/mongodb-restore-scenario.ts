// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * MongoDB backup + restore lifecycle against a live Temps instance.
 *
 * This scenario exercises the NEW `restore_in_place` path added in
 * crates/temps-providers/src/externalsvc/mongodb.rs — the first time MongoDB
 * has had real, verified restore capability (not just backup).
 *
 * Flow:
 *   1. Provision a real standalone MongoDB service.
 *   2. Insert 3 recognizable "pre-backup" documents via `docker exec mongosh`.
 *   3. Create an S3 source (MinIO) and trigger a real backup via the backup
 *      engine (`MongodbEngine` sidecar: mongodump --archive --gzip → S3).
 *   4. Poll the backup to a real `completed` state.
 *   5. Insert 2 more "post-backup" documents (distinct _id / field values).
 *   6. Verify all 5 documents are present before restore (sanity check).
 *   7. Start an in-place restore (`POST /external-services/{id}/restore`,
 *      mode: "in_place") and poll `GET /restore-runs/{id}` to `completed`.
 *   8. Read back documents via the read-only data-browser API
 *      (`readEntityRows`): assert the 3 pre-backup documents are present with
 *      the correct field values AND the 2 post-backup documents are ABSENT.
 *      This is the key correctness assertion — `--drop` actually reverted
 *      state, not just "merged" with it.
 *   9. Teardown (service, S3 source).
 *
 * Needs:
 * - MinIO running (from docker-compose.e2e.yml, port 9092).
 * - A bucket named `temps-e2e-backups` already created in MinIO.
 * - Docker accessible from the host running this test (for the mongosh exec).
 */

import {
  createS3Source,
  deleteS3Source,
  runExternalServiceBackup,
  listExternalServiceBackups,
  getBackup,
  startRestore,
  getRestoreRun,
  readEntityRows,
  getService,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eService,
  pollUntil,
  teardown,
  makeRunId,
  sleep,
} from '../lib/flows.ts'

export interface MongodbRestoreScenarioOptions {
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

interface MongodbRestoreScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

// The database and collection we write documents into.
// Using a single-word ASCII database name avoids URL-encoding issues.
const MONGO_DATABASE = 'e2etest'
const MONGO_COLLECTION = 'restore_probe'

// Pre-backup documents: inserted before the backup, must survive restore.
const PRE_BACKUP_DOCS = [
  { _id: 'pre-1', label: 'pre-backup-alpha', value: 100 },
  { _id: 'pre-2', label: 'pre-backup-beta', value: 200 },
  { _id: 'pre-3', label: 'pre-backup-gamma', value: 300 },
]

// Post-backup documents: inserted after the backup, must be ABSENT after restore.
const POST_BACKUP_DOCS = [
  { _id: 'post-1', label: 'post-backup-delta', value: 400 },
  { _id: 'post-2', label: 'post-backup-epsilon', value: 500 },
]

/**
 * Run `docker exec <container> mongosh --quiet --authenticationDatabase admin
 *   -u <user> -p <pass> --eval <script> <database>`.
 *
 * Credentials are passed as separate argv tokens (not through a shell string),
 * so special characters cannot cause injection.  Returns stdout trimmed.
 * Throws on non-zero exit.
 */
async function mongoshExec(
  containerName: string,
  username: string,
  password: string,
  database: string,
  script: string,
): Promise<string> {
  const proc = Bun.spawn(
    [
      'docker',
      'exec',
      containerName,
      'mongosh',
      '--quiet',
      '--authenticationDatabase',
      'admin',
      '-u',
      username,
      '-p',
      password,
      '--eval',
      script,
      database,
    ],
    { stdout: 'pipe', stderr: 'pipe' },
  )
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (code !== 0) {
    throw new Error(
      `mongosh exec in '${containerName}' exited ${code}.\nstdout: ${stdout.trim()}\nstderr: ${stderr.trim()}`,
    )
  }
  return stdout.trim()
}

/**
 * Insert a batch of documents into the collection in one `insertMany` call.
 * Documents must already be serialisable by JSON.stringify.
 */
async function insertDocuments(
  containerName: string,
  username: string,
  password: string,
  documents: Array<Record<string, unknown>>,
): Promise<void> {
  const docsJson = JSON.stringify(documents)
  const script = `db.getCollection('${MONGO_COLLECTION}').insertMany(${docsJson})`
  await mongoshExec(containerName, username, password, MONGO_DATABASE, script)
}

/**
 * Return the count of documents in the collection where `_id` is in the given
 * set.  Used to verify which documents survived restore.
 */
async function countDocumentsByIds(
  containerName: string,
  username: string,
  password: string,
  ids: string[],
): Promise<number> {
  const idsJson = JSON.stringify(ids)
  const script = `db.getCollection('${MONGO_COLLECTION}').countDocuments({_id: {$in: ${idsJson}}})`
  const out = await mongoshExec(containerName, username, password, MONGO_DATABASE, script)
  const n = parseInt(out, 10)
  if (isNaN(n)) throw new Error(`countDocuments returned non-numeric: "${out}"`)
  return n
}

/**
 * Read documents via the data-browser API (read-only, no Docker exec).
 * Returns the raw rows array so callers can inspect fields / _id values.
 *
 * MongoDB path layout in the data-browser:
 *   depth-1 path = database name
 *   entity       = collection name
 */
async function readCollectionViaApi(
  client: Parameters<typeof readEntityRows>[0]['client'],
  serviceId: number,
  database: string,
  collection: string,
  limit = 50,
): Promise<Array<Record<string, unknown>>> {
  const result = unwrap(
    await readEntityRows({
      client,
      path: { service_id: serviceId, path: database, entity: collection },
      query: { limit, sort_by: '_id', sort_order: 'asc' },
    }),
    'readEntityRows(mongodb)',
  )
  return (result.rows ?? []) as Array<Record<string, unknown>>
}

export async function mongodbRestoreScenarioCommand(
  opts: MongodbRestoreScenarioOptions,
): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }

  if (!json) log(`Temps MongoDB restore scenario  ->  ${cfg.url}`)

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

  const serviceIds: number[] = []
  let s3SourceId: number | undefined
  let mongoContainerName: string | undefined
  let mongoUsername: string | undefined
  let mongoPassword: string | undefined

  try {
    // ── Step 1: provision MongoDB service ─────────────────────────────────
    const service = await step('provision a real standalone MongoDB service', () =>
      createE2eService(client, {
        name: `${runId}-mongo`,
        serviceType: 'mongodb',
        parameters: {
          database: MONGO_DATABASE,
          username: 'root',
        },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    // Fetch the full service record to get the auto-generated parameters
    // (password, port, container_name). `createE2eService` only returns id+name.
    // The API returns `ExternalServiceDetails` with `current_parameters` (masked)
    // and `service.id`/`service.name`.  Sensitive fields like `password` are
    // masked to "***" in `current_parameters`; we still read them to discover
    // which fields exist, but for the password we rely on the fact that the
    // container's own MONGO_INITDB_ROOT_PASSWORD env var is accessible via
    // `docker exec`, and we can also extract the real password by calling docker
    // inspect directly.  However, the simpler approach: call `docker inspect`
    // on the container to read MONGO_INITDB_ROOT_PASSWORD.
    //
    // Note: current_parameters may mask sensitive values; the container name
    // comes from the service name and is predictable.
    unwrap(await getService({ client, path: { id: service.id } }), 'getService')
    const svcName = `${runId}-mongo`
    mongoContainerName = `temps-mongodb-${svcName}`
    mongoUsername = 'root'
    // Read the actual password from the container's env vars via docker inspect.
    mongoPassword = await (async () => {
      const proc = Bun.spawn(
        ['docker', 'inspect', '--format', '{{range .Config.Env}}{{println .}}{{end}}', mongoContainerName!],
        { stdout: 'pipe', stderr: 'pipe' },
      )
      const [out, , code] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ])
      if (code !== 0) throw new Error(`docker inspect failed for container ${mongoContainerName}`)
      for (const line of out.split('\n')) {
        if (line.startsWith('MONGO_INITDB_ROOT_PASSWORD=')) {
          return line.slice('MONGO_INITDB_ROOT_PASSWORD='.length).trim()
        }
      }
      throw new Error(`MONGO_INITDB_ROOT_PASSWORD not found in container env vars for ${mongoContainerName}`)
    })()

    log(`  container: ${mongoContainerName}`)
    log(`  username: ${mongoUsername}`)

    // ── Step 2: insert pre-backup documents ────────────────────────────────
    await step('insert 3 pre-backup documents via docker exec mongosh', async () => {
      await insertDocuments(mongoContainerName!, mongoUsername!, mongoPassword!, PRE_BACKUP_DOCS)
      const count = await countDocumentsByIds(
        mongoContainerName!,
        mongoUsername!,
        mongoPassword!,
        PRE_BACKUP_DOCS.map((d) => d._id),
      )
      if (count !== 3) {
        throw new Error(`expected 3 pre-backup documents, found ${count}`)
      }
    })

    // ── Step 3: create S3 source + trigger backup ──────────────────────────
    const source = await step('create an S3 source pointed at the local MinIO', async () => {
      return unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-minio-mongo`,
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
    })
    s3SourceId = source.id
    log(`  s3 source #${source.id}`)

    await step('trigger a real mongodump backup (MongodbEngine sidecar → MinIO)', async () => {
      unwrap(
        await runExternalServiceBackup({
          client,
          path: { id: service.id },
          body: { s3_source_id: source.id, backup_type: 'full' },
        }),
        'runExternalServiceBackup',
      )
    })

    // ── Step 4: poll backup to completed ──────────────────────────────────
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
        { timeoutMs: 60_000, intervalMs: 2000, label: 'backup row to appear' },
      )
      const entry = list.backups[0]!
      const finalBackup = await pollUntil(
        async () =>
          unwrap(await getBackup({ client, path: { id: entry.backup_id } }), 'getBackup'),
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
    log(`  backup #${backupEntry.id} (backup_id=${backupEntry.backup_id})`)

    // ── Step 5: insert post-backup documents ───────────────────────────────
    await step('insert 2 post-backup documents (must be ABSENT after restore)', async () => {
      await insertDocuments(mongoContainerName!, mongoUsername!, mongoPassword!, POST_BACKUP_DOCS)
      const count = await countDocumentsByIds(
        mongoContainerName!,
        mongoUsername!,
        mongoPassword!,
        POST_BACKUP_DOCS.map((d) => d._id),
      )
      if (count !== 2) {
        throw new Error(`expected 2 post-backup documents, found ${count}`)
      }
    })

    // ── Step 6: sanity-check all 5 docs present before restore ────────────
    await step('verify all 5 documents (3 pre + 2 post) are present before restore', async () => {
      const allIds = [...PRE_BACKUP_DOCS, ...POST_BACKUP_DOCS].map((d) => d._id)
      const count = await countDocumentsByIds(
        mongoContainerName!,
        mongoUsername!,
        mongoPassword!,
        allIds,
      )
      if (count !== 5) {
        throw new Error(`expected 5 total documents before restore, found ${count}`)
      }
    })

    // ── Step 7: start in-place restore ────────────────────────────────────
    const restoreRun = await step(
      'start an in-place restore from the backup taken before post-backup docs',
      async () => {
        return unwrap(
          await startRestore({
            client,
            path: { id: service.id },
            body: {
              backup_id: backupEntry.id,
              mode: 'in_place',
            },
          }),
          'startRestore',
        )
      },
    )
    log(`  restore run #${restoreRun.id}`)

    await step('poll the restore run to a real completed state', async () => {
      const finalRun = await pollUntil(
        async () =>
          unwrap(await getRestoreRun({ client, path: { id: restoreRun.id } }), 'getRestoreRun'),
        (r) => r.status === 'completed' || r.status === 'failed',
        {
          timeoutMs: 240_000,
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

    // Give mongod a moment to settle after restore before querying.
    await sleep(3_000)

    // ── Step 8: verify correctness via data-browser API ───────────────────
    await step(
      'data-browser API: 3 pre-backup docs present with correct values, 2 post-backup docs absent',
      async () => {
        const rows = await readCollectionViaApi(client, service.id, MONGO_DATABASE, MONGO_COLLECTION)
        log(`  data-browser returned ${rows.length} document(s) in collection`)

        // Map rows by _id for easy lookup.
        // The data-browser can return the document either flat (with _id at top
        // level) or nested under a "document" key depending on the engine
        // serialisation.  Handle both shapes.
        const byId = new Map<string, Record<string, unknown>>()
        for (const row of rows) {
          const nested = row['document'] as Record<string, unknown> | undefined
          const id = String(row['_id'] ?? nested?.['_id'] ?? '')
          byId.set(id, row)
        }

        // Pre-backup documents must be present with correct values.
        for (const expected of PRE_BACKUP_DOCS) {
          const row = byId.get(expected._id)
          if (!row) {
            throw new Error(
              `pre-backup document '${expected._id}' is MISSING after restore — ` +
                'restore_in_place did not revert to backup state',
            )
          }
          // Check the label field — may be flat or nested under "document".
          const nested = row['document'] as Record<string, unknown> | undefined
          const label = (row['label'] ?? nested?.['label']) as string | undefined
          if (label !== expected.label) {
            throw new Error(
              `pre-backup doc '${expected._id}' has label="${label}", expected "${expected.label}"`,
            )
          }
          log(`  ✓ pre-backup doc '${expected._id}' present with correct label`)
        }

        // Post-backup documents must be absent (--drop removed them).
        for (const absent of POST_BACKUP_DOCS) {
          if (byId.has(absent._id)) {
            throw new Error(
              `post-backup document '${absent._id}' is STILL PRESENT after restore — ` +
                'mongorestore --drop did not revert state; this is a correctness failure',
            )
          }
          log(`  ✓ post-backup doc '${absent._id}' correctly absent`)
        }

        if (rows.length !== PRE_BACKUP_DOCS.length) {
          throw new Error(
            `after restore, collection has ${rows.length} document(s), ` +
              `expected exactly ${PRE_BACKUP_DOCS.length} (the 3 pre-backup docs)`,
          )
        }
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'})`,
      )
    } else {
      if (s3SourceId) {
        await deleteS3Source({ client, path: { id: s3SourceId } }).catch(() => undefined)
      }
      const td = await teardown(client, { deployments: [], projectIds: [], serviceIds })
      log(`\n▶ teardown: removed ${td.deletedServices} service(s)`)
    }
  }

  const ok = steps.every((s) => s.ok)
  const result: MongodbRestoreScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    const total = steps.length
    const passed = steps.filter((s) => s.ok).length
    log(`\n${ok ? '✅' : '❌'} MongoDB restore scenario: ${passed}/${total} steps passed`)
    if (!ok) {
      for (const s of steps.filter((s) => !s.ok)) {
        log(`  ✗ ${s.step}: ${s.detail ?? '(no detail)'}`)
      }
    }
  }

  if (!ok) process.exitCode = 1
}
