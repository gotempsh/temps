// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * MariaDB point-in-time recovery against a live Temps instance.
 *
 * `mariadb-restore-scenario` already proves plain in-place restore. This
 * scenario proves the PITR-specific path: `MariaDbService::restore_pitr`
 * (`crates/temps-providers/src/externalsvc/mariadb.rs`), which downloads a
 * physical `mariadb-backup` base, replays archived binlog segments up to a
 * `recovery_target_time`, and stops before crossing it.
 *
 * Unlike managed Postgres (which archives WAL on every container via a fixed
 * `archive_timeout`, see `pitr-scenario.ts`), MariaDB binlog archiving is
 * driven by `HealthMonitor`'s periodic MariaDB check
 * (`crates/temps-providers/src/health_monitor.rs`), and only runs at all for
 * a service once an ENABLED `backup_schedules` row covers it with an
 * `s3_source_id` (`find_s3_source_for_service`). There is no HTTP endpoint to
 * trigger binlog archiving on demand, so this scenario creates a real backup
 * schedule (with a `schedule_expression` that never actually fires — we don't
 * need the schedule's own cron to run, only the row to exist and be enabled)
 * and waits out a real archive cycle.
 *
 * Flow:
 *   1. Provision a real standalone MariaDB service with
 *      `binlog_archive_interval: "1m"` so the health-monitor's gate clears
 *      quickly instead of the 5m default.
 *   2. Create an S3 source (MinIO) and a real backup schedule covering this
 *      service, enabled — this is what unlocks binlog archiving for it.
 *   3. Insert 3 "T1" rows via `docker exec mariadb`.
 *   4. Trigger a real ad-hoc physical backup (`mariadb_physical`, auto-
 *      selected because `mariadb-backup`/`mariadb-binlog` are present in the
 *      stock `mariadb:lts` image) and poll it to `completed`. This is the
 *      PITR base.
 *   5. Wait out a real binlog-archive cycle so T1's segment lands in S3
 *      (`HealthMonitor` ticks every `poll_interval_secs` (30s default) and
 *      only archives once `binlog_archive_interval` (60s here) has elapsed
 *      since the last archive for this service — 100s covers both with
 *      margin).
 *   6. Capture the PITR recovery-target timestamp (after T1, before T2).
 *   7. Insert 2 "T2" rows the restore must NOT include.
 *   8. Start an in-place PITR restore (`mode: "pitr"`, `to_new_service:
 *      false`) targeting the captured timestamp, and poll to `completed`.
 *   9. Read back via the read-only data-browser API: assert the 3 T1 rows
 *      are present with correct values AND the 2 T2 rows are absent.
 *  10. Teardown (backup schedule, S3 source, service).
 *
 * Needs:
 * - MinIO running (from docker-compose.e2e.yml, port 9092).
 * - A bucket named `temps-e2e-backups` already created in MinIO.
 * - Docker accessible from the host running this test (for the mariadb exec).
 */

import {
  createS3Source,
  deleteS3Source,
  createBackupSchedule,
  attachScheduleServices,
  deleteBackupSchedule,
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

export interface MariadbPitrScenarioOptions {
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

interface MariadbPitrScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

const MARIADB_DATABASE = 'e2etest'
const MARIADB_TABLE = 'pitr_probe'

interface ProbeRow {
  id: number
  label: string
  value: number
}

// T1 rows: inserted before the PITR target time, must survive the restore.
const T1_ROWS: ProbeRow[] = [
  { id: 1, label: 't1-alpha', value: 100 },
  { id: 2, label: 't1-beta', value: 200 },
  { id: 3, label: 't1-gamma', value: 300 },
]

// T2 rows: inserted after the PITR target time, must be ABSENT after restore.
const T2_ROWS: ProbeRow[] = [
  { id: 4, label: 't2-delta', value: 400 },
  { id: 5, label: 't2-epsilon', value: 500 },
]

function sqlQuote(value: string): string {
  return `'${value.replace(/'/g, "''")}'`
}

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

export async function mariadbPitrScenarioCommand(
  opts: MariadbPitrScenarioOptions,
): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }

  if (!json) log(`Temps MariaDB PITR scenario  ->  ${cfg.url}`)

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
  let scheduleId: number | undefined
  let mariadbContainerName: string | undefined
  let mariadbRootPassword: string | undefined
  let pitrTargetTime: string | undefined

  try {
    // ── Step 1: provision MariaDB with fast binlog archiving ──────────────
    const svcName = `${runId}-mariadb-pitr`
    const service = await step(
      'provision a real standalone MariaDB service (binlog_archive_interval=1m)',
      () =>
        createE2eService(client, {
          name: svcName,
          serviceType: 'mariadb',
          parameters: {
            database: MARIADB_DATABASE,
            username: 'app',
            binlog_archive_interval: '1m',
          },
        }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    unwrap(await getService({ client, path: { id: service.id } }), 'getService')

    // Container name is derived as `mariadb-{name}` (see
    // `MariaDbService::get_container_name` in externalsvc/mariadb.rs).
    mariadbContainerName = `mariadb-${svcName}`

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

    // ── Step 2: S3 source + backup schedule (unlocks binlog archiving) ────
    const source = await step('create an S3 source pointed at the local MinIO', async () => {
      return unwrap(
        await createS3Source({
          client,
          body: {
            name: `${runId}-minio-mariadb-pitr`,
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

    // `HealthMonitor::find_s3_source_for_service` only requires an ENABLED
    // schedule covering this service with an `s3_source_id` -- it does not
    // need the schedule's own cron to ever fire. Use a `schedule_expression`
    // that can never match (Feb 31st does not exist) so this schedule never
    // triggers its own backup run during the test. The schedule parser is
    // 6-field (seconds-first: sec min hour day month weekday).
    const schedule = await step(
      'create an enabled backup schedule covering this service (unlocks binlog archiving)',
      async () => {
        const created = unwrap(
          await createBackupSchedule({
            client,
            body: {
              name: `${runId}-mariadb-pitr-schedule`,
              backup_type: 'full',
              schedule_expression: '0 0 0 31 2 *',
              retention_period: 1,
              enabled: true,
              target_all_services: false,
              s3_source_id: source.id,
              tags: [],
            },
          }),
          'createBackupSchedule',
        )
        unwrap(
          await attachScheduleServices({
            client,
            path: { id: created.id },
            body: { service_ids: [service.id] },
          }),
          'attachScheduleServices',
        )
        return created
      },
    )
    scheduleId = schedule.id
    log(`  backup schedule #${schedule.id}`)

    // ── Step 3: insert T1 rows ─────────────────────────────────────────────
    await step('insert 3 T1 rows via docker exec mariadb', async () => {
      await insertRows(mariadbContainerName!, mariadbRootPassword!, T1_ROWS)
      const count = await countRowsByIds(
        mariadbContainerName!,
        mariadbRootPassword!,
        T1_ROWS.map((r) => r.id),
      )
      if (count !== 3) {
        throw new Error(`expected 3 T1 rows, found ${count}`)
      }
    })

    // ── Step 4: ad-hoc physical backup (the PITR base) ─────────────────────
    await step(
      'trigger a real MariaDB physical backup (the PITR base)',
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

    // ── Step 5: wait out a real binlog-archive cycle ───────────────────────
    // `HealthMonitor` polls every `poll_interval_secs` (30s default) and only
    // archives once `binlog_archive_interval` (60s, set above) has elapsed
    // since the last archive attempt for this service -- 100s covers both
    // with margin, mirroring `pitr-scenario.ts`'s WAL-archive wait for
    // managed postgres.
    await step("wait for T1's binlog segment to be archived (interval + poll margin)", async () => {
      await sleep(100_000)
    })

    // ── Step 6: capture the PITR target ────────────────────────────────────
    pitrTargetTime = await step('capture the PITR recovery-target timestamp (after T1, before T2)', async () => {
      const iso = new Date().toISOString()
      log(`  target time: ${iso}`)
      return iso
    })

    await step('buffer so T2 is unambiguously after the captured target time', async () => {
      await sleep(3_000)
    })

    // ── Step 7: insert T2 rows ──────────────────────────────────────────────
    await step('insert 2 T2 rows (must be ABSENT after PITR restore)', async () => {
      await insertRows(mariadbContainerName!, mariadbRootPassword!, T2_ROWS)
      const count = await countRowsByIds(
        mariadbContainerName!,
        mariadbRootPassword!,
        T2_ROWS.map((r) => r.id),
      )
      if (count !== 2) {
        throw new Error(`expected 2 T2 rows, found ${count}`)
      }
    })

    // ── Step 8: start the PITR restore, in place ───────────────────────────
    const restoreRun = await step(
      'start an in-place PITR restore to the captured target time',
      async () => {
        return unwrap(
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
      },
    )
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
        throw new Error(
          `restore run ${restoreRun.id} ended in status "${finalRun.status}": ${finalRun.error_message}`,
        )
      }
    })

    // Give mariadbd a moment to settle after restore before querying.
    await sleep(3_000)

    // ── Step 9: verify correctness via data-browser API ────────────────────
    await step(
      'data-browser API: 3 T1 rows present with correct values, 2 T2 rows absent',
      async () => {
        const rows = await readTableViaApi(client, service.id)
        log(`  data-browser returned ${rows.length} row(s) in table`)

        const byId = new Map<string, Record<string, unknown>>()
        for (const row of rows) {
          byId.set(String(row['id']), row)
        }

        for (const expected of T1_ROWS) {
          const row = byId.get(String(expected.id))
          if (!row) {
            throw new Error(
              `T1 row id=${expected.id} is MISSING after PITR restore — ` +
                'restore_pitr did not replay up to the target time',
            )
          }
          const label = row['label'] as string | undefined
          const value = Number(row['value'])
          if (label !== expected.label || value !== expected.value) {
            throw new Error(
              `T1 row id=${expected.id} has label="${label}" value=${value}, ` +
                `expected label="${expected.label}" value=${expected.value}`,
            )
          }
          log(`  ✓ T1 row id=${expected.id} present with correct values`)
        }

        for (const absent of T2_ROWS) {
          if (byId.has(String(absent.id))) {
            throw new Error(
              `T2 row id=${absent.id} is STILL PRESENT after PITR restore — ` +
                'restore replayed past the target time; this is a correctness failure',
            )
          }
          log(`  ✓ T2 row id=${absent.id} correctly absent`)
        }

        if (rows.length !== T1_ROWS.length) {
          throw new Error(
            `after PITR restore, table has ${rows.length} row(s), ` +
              `expected exactly ${T1_ROWS.length} (the 3 T1 rows)`,
          )
        }
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: services=${serviceIds.join(',')} s3Source=${s3SourceId ?? '-'} schedule=${scheduleId ?? '-'})`,
      )
    } else {
      if (scheduleId) {
        await deleteBackupSchedule({ client, path: { id: scheduleId } }).catch(() => undefined)
      }
      if (s3SourceId) {
        await deleteS3Source({ client, path: { id: s3SourceId } }).catch(() => undefined)
      }
      const td = await teardown(client, { deployments: [], projectIds: [], serviceIds })
      log(`\n▶ teardown: removed ${td.deletedServices} service(s)`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: MariadbPitrScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    const total = steps.length
    const passed = steps.filter((s) => s.ok).length
    log(`\n${ok ? '✅' : '❌'} MariaDB PITR scenario: ${passed}/${total} steps passed`)
    if (!ok) {
      for (const s of steps.filter((s) => !s.ok)) {
        log(`  ✗ ${s.step}: ${s.detail ?? '(no detail)'}`)
      }
    }
  }

  if (!ok) process.exitCode = 1
}
