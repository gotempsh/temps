// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * MariaDB backup + restore lifecycle against a live Temps instance.
 *
 * MariaDB restore is fully implemented (`crates/temps-providers/src/
 * externalsvc/mariadb.rs`): `restore_capabilities()` reports
 * `restore_in_place: true`, `restore_to_new_service: true`, `pitr: true`,
 * and the default `restore_in_place` trait method dispatches to
 * `restore_from_s3`, which auto-detects a physical (`mariadb-backup`
 * mbstream) base vs a logical `mariadb-dump` and restores accordingly. This
 * scenario proves the in-place path end to end with genuine before/after
 * data verification — not just that the API calls returned 2xx.
 *
 * Flow:
 *   1. Provision a real standalone MariaDB service.
 *   2. Insert 3 recognizable "pre-backup" rows via `docker exec mariadb`
 *      (the real CLI client shipped in the `mariadb:lts` image — see
 *      `MariaDbService::execute_sql`'s own `command -v mariadb` check at
 *      mariadb.rs:880).
 *   3. Create an S3 source (MinIO) and trigger a real backup via the backup
 *      engine (`crates/temps-backup/src/engines/dispatch.rs` auto-selects
 *      `mariadb_physical` when `mariadb-backup`/`mariadb-binlog` are present
 *      in the container, which they are in the stock `mariadb:lts` image).
 *   4. Poll the backup to a real `completed` state.
 *   5. Insert 2 more "post-backup" rows (distinct id / column values).
 *   6. Verify all 5 rows are present before restore (sanity check).
 *   7. Start an in-place restore (`POST /external-services/{id}/restore`,
 *      mode: "in_place") and poll `GET /restore-runs/{id}` to `completed`.
 *   8. Read back rows via the read-only data-browser API (`readEntityRows`):
 *      assert the 3 pre-backup rows are present with the correct column
 *      values AND the 2 post-backup rows are ABSENT. This is the key
 *      correctness assertion — the restore actually reverted state, not
 *      just left current state in place.
 *   9. Teardown (service, S3 source).
 *
 * Needs:
 * - MinIO running (from docker-compose.e2e.yml, port 9092).
 * - A bucket named `temps-e2e-backups` already created in MinIO.
 * - Docker accessible from the host running this test (for the mariadb exec).
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

export interface MariadbRestoreScenarioOptions {
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

interface MariadbRestoreScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

// The database and table we write rows into.
const MARIADB_DATABASE = 'e2etest'
const MARIADB_TABLE = 'restore_probe'

interface ProbeRow {
  id: number
  label: string
  value: number
}

// Pre-backup rows: inserted before the backup, must survive restore.
const PRE_BACKUP_ROWS: ProbeRow[] = [
  { id: 1, label: 'pre-backup-alpha', value: 100 },
  { id: 2, label: 'pre-backup-beta', value: 200 },
  { id: 3, label: 'pre-backup-gamma', value: 300 },
]

// Post-backup rows: inserted after the backup, must be ABSENT after restore.
const POST_BACKUP_ROWS: ProbeRow[] = [
  { id: 4, label: 'post-backup-delta', value: 400 },
  { id: 5, label: 'post-backup-epsilon', value: 500 },
]

/** Escape a single-quoted SQL string literal (our own controlled values only). */
function sqlQuote(value: string): string {
  return `'${value.replace(/'/g, "''")}'`
}

/**
 * Run `docker exec <container> mariadb -uroot -p<password> -N -B -e <script>
 *   <database>`.
 *
 * `-N -B` (skip column names, batch/tab-separated output) makes results easy
 * to parse. Credentials are passed as separate argv tokens (not through a
 * shell string), so special characters cannot cause injection. Returns
 * stdout trimmed. Throws on non-zero exit.
 */
async function mariadbExec(
  containerName: string,
  rootPassword: string,
  database: string,
  script: string,
): Promise<string> {
  const proc = Bun.spawn(
    [
      'docker',
      'exec',
      containerName,
      'mariadb',
      '-uroot',
      `-p${rootPassword}`,
      '-N',
      '-B',
      '-e',
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
      `mariadb exec in '${containerName}' exited ${code}.\nstdout: ${stdout.trim()}\nstderr: ${stderr.trim()}`,
    )
  }
  return stdout.trim()
}

/** Create the probe table (idempotent) and insert a batch of rows in one INSERT. */
async function insertRows(
  containerName: string,
  rootPassword: string,
  rows: ProbeRow[],
): Promise<void> {
  const values = rows
    .map((r) => `(${r.id}, ${sqlQuote(r.label)}, ${r.value})`)
    .join(', ')
  const script =
    `CREATE TABLE IF NOT EXISTS ${MARIADB_TABLE} ` +
    `(id INT PRIMARY KEY, label VARCHAR(255) NOT NULL, value INT NOT NULL); ` +
    `INSERT INTO ${MARIADB_TABLE} (id, label, value) VALUES ${values};`
  await mariadbExec(containerName, rootPassword, MARIADB_DATABASE, script)
}

/** Count rows in the probe table whose `id` is in the given set. */
async function countRowsByIds(
  containerName: string,
  rootPassword: string,
  ids: number[],
): Promise<number> {
  const script = `SELECT COUNT(*) FROM ${MARIADB_TABLE} WHERE id IN (${ids.join(',')});`
  const out = await mariadbExec(containerName, rootPassword, MARIADB_DATABASE, script)
  const n = parseInt(out, 10)
  if (isNaN(n)) throw new Error(`SELECT COUNT(*) returned non-numeric: "${out}"`)
  return n
}

/**
 * Read rows via the data-browser API (read-only, no Docker exec).
 *
 * MariaDB path layout in the data-browser: depth-1 path = database name,
 * entity = table name (see `database_from_path` in
 * `crates/temps-providers/src/mariadb_query.rs`).
 */
async function readTableViaApi(
  client: Parameters<typeof readEntityRows>[0]['client'],
  serviceId: number,
  limit = 50,
): Promise<Array<Record<string, unknown>>> {
  const result = unwrap(
    await readEntityRows({
      client,
      path: { service_id: serviceId, path: MARIADB_DATABASE, entity: MARIADB_TABLE },
      query: { limit, sort_by: 'id', sort_order: 'asc' },
    }),
    'readEntityRows(mariadb)',
  )
  return (result.rows ?? []) as Array<Record<string, unknown>>
}

export async function mariadbRestoreScenarioCommand(
  opts: MariadbRestoreScenarioOptions,
): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }

  if (!json) log(`Temps MariaDB restore scenario  ->  ${cfg.url}`)

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
  let mariadbContainerName: string | undefined
  let mariadbRootPassword: string | undefined

  try {
    // ── Step 1: provision MariaDB service ─────────────────────────────────
    const svcName = `${runId}-mariadb`
    const service = await step('provision a real standalone MariaDB service', () =>
      createE2eService(client, {
        name: svcName,
        serviceType: 'mariadb',
        parameters: {
          database: MARIADB_DATABASE,
          username: 'app',
        },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    unwrap(await getService({ client, path: { id: service.id } }), 'getService')

    // Container name is derived as `mariadb-{name}` (see
    // `MariaDbService::get_container_name` in externalsvc/mariadb.rs).
    mariadbContainerName = `mariadb-${svcName}`

    // Read the real root password from the container's env vars via docker
    // inspect -- `current_parameters` on the service masks it to "***".
    mariadbRootPassword = await (async () => {
      const proc = Bun.spawn(
        [
          'docker',
          'inspect',
          '--format',
          '{{range .Config.Env}}{{println .}}{{end}}',
          mariadbContainerName!,
        ],
        { stdout: 'pipe', stderr: 'pipe' },
      )
      const [out, , code] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ])
      if (code !== 0) throw new Error(`docker inspect failed for container ${mariadbContainerName}`)
      for (const line of out.split('\n')) {
        if (line.startsWith('MARIADB_ROOT_PASSWORD=')) {
          return line.slice('MARIADB_ROOT_PASSWORD='.length).trim()
        }
      }
      throw new Error(
        `MARIADB_ROOT_PASSWORD not found in container env vars for ${mariadbContainerName}`,
      )
    })()

    log(`  container: ${mariadbContainerName}`)

    // ── Step 2: insert pre-backup rows ─────────────────────────────────────
    await step('insert 3 pre-backup rows via docker exec mariadb', async () => {
      await insertRows(mariadbContainerName!, mariadbRootPassword!, PRE_BACKUP_ROWS)
      const count = await countRowsByIds(
        mariadbContainerName!,
        mariadbRootPassword!,
        PRE_BACKUP_ROWS.map((r) => r.id),
      )
      if (count !== 3) {
        throw new Error(`expected 3 pre-backup rows, found ${count}`)
      }
    })

    // ── Step 3: create S3 source + trigger backup ──────────────────────────
    const source = await step('create an S3 source pointed at the local MinIO', async () => {
      return unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-minio-mariadb`,
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

    await step(
      'trigger a real MariaDB backup (mariadb-backup/mariadb-dump → MinIO)',
      async () => {
        unwrap(
          await runExternalServiceBackup({
            client,
            path: { id: service.id },
            body: { s3_source_id: source.id, backup_type: 'full' },
          }),
          'runExternalServiceBackup',
        )
      },
    )

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
    log(`  backup #${backupEntry.id} (backup_id=${backupEntry.backup_id})`)

    // ── Step 5: insert post-backup rows ────────────────────────────────────
    await step('insert 2 post-backup rows (must be ABSENT after restore)', async () => {
      await insertRows(mariadbContainerName!, mariadbRootPassword!, POST_BACKUP_ROWS)
      const count = await countRowsByIds(
        mariadbContainerName!,
        mariadbRootPassword!,
        POST_BACKUP_ROWS.map((r) => r.id),
      )
      if (count !== 2) {
        throw new Error(`expected 2 post-backup rows, found ${count}`)
      }
    })

    // ── Step 6: sanity-check all 5 rows present before restore ────────────
    await step('verify all 5 rows (3 pre + 2 post) are present before restore', async () => {
      const allIds = [...PRE_BACKUP_ROWS, ...POST_BACKUP_ROWS].map((r) => r.id)
      const count = await countRowsByIds(mariadbContainerName!, mariadbRootPassword!, allIds)
      if (count !== 5) {
        throw new Error(`expected 5 total rows before restore, found ${count}`)
      }
    })

    // ── Step 7: start in-place restore ────────────────────────────────────
    const restoreRun = await step(
      'start an in-place restore from the backup taken before post-backup rows',
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
        async () => unwrap(await getRestoreRun({ client, path: { id: restoreRun.id } }), 'getRestoreRun'),
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

    // Give mariadbd a moment to settle after restore before querying.
    await sleep(3_000)

    // ── Step 8: verify correctness via data-browser API ───────────────────
    await step(
      'data-browser API: 3 pre-backup rows present with correct values, 2 post-backup rows absent',
      async () => {
        const rows = await readTableViaApi(client, service.id)
        log(`  data-browser returned ${rows.length} row(s) in table`)

        const byId = new Map<string, Record<string, unknown>>()
        for (const row of rows) {
          byId.set(String(row['id']), row)
        }

        // Pre-backup rows must be present with correct column values.
        for (const expected of PRE_BACKUP_ROWS) {
          const row = byId.get(String(expected.id))
          if (!row) {
            throw new Error(
              `pre-backup row id=${expected.id} is MISSING after restore — ` +
                'restore_in_place did not revert to backup state',
            )
          }
          const label = row['label'] as string | undefined
          const value = Number(row['value'])
          if (label !== expected.label || value !== expected.value) {
            throw new Error(
              `pre-backup row id=${expected.id} has label="${label}" value=${value}, ` +
                `expected label="${expected.label}" value=${expected.value}`,
            )
          }
          log(`  ✓ pre-backup row id=${expected.id} present with correct values`)
        }

        // Post-backup rows must be absent (restore reverted to the pre-backup base).
        for (const absent of POST_BACKUP_ROWS) {
          if (byId.has(String(absent.id))) {
            throw new Error(
              `post-backup row id=${absent.id} is STILL PRESENT after restore — ` +
                'restore did not revert state; this is a correctness failure',
            )
          }
          log(`  ✓ post-backup row id=${absent.id} correctly absent`)
        }

        if (rows.length !== PRE_BACKUP_ROWS.length) {
          throw new Error(
            `after restore, table has ${rows.length} row(s), ` +
              `expected exactly ${PRE_BACKUP_ROWS.length} (the 3 pre-backup rows)`,
          )
        }
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'})`)
    } else {
      if (s3SourceId) {
        await deleteS3Source({ client, path: { id: s3SourceId } }).catch(() => undefined)
      }
      const td = await teardown(client, { deployments: [], projectIds: [], serviceIds })
      log(`\n▶ teardown: removed ${td.deletedServices} service(s)`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: MariadbRestoreScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    const total = steps.length
    const passed = steps.filter((s) => s.ok).length
    log(`\n${ok ? '✅' : '❌'} MariaDB restore scenario: ${passed}/${total} steps passed`)
    if (!ok) {
      for (const s of steps.filter((s) => !s.ok)) {
        log(`  ✗ ${s.step}: ${s.detail ?? '(no detail)'}`)
      }
    }
  }

  if (!ok) process.exitCode = 1
}
