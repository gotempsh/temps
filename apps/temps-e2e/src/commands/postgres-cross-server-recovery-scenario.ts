// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import { closeSync, copyFileSync, mkdirSync, mkdtempSync, openSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { confirmedFullBackup } from '../lib/backup-run.ts'
import { pollUntil } from '../lib/flows.ts'

type Cleanup = () => void | Promise<unknown>
interface Listing {
  backups: { source: string; format: string; location: string }[]
  scan_errors: string[]
}

function command(argv: string[], env = process.env) {
  const result = Bun.spawnSync(argv, { env, timeout: 240_000, stderr: 'pipe', stdout: 'pipe' })
  if (!result.success)
    throw new Error(
      `${argv[0]} ${argv[1]} failed (exit ${result.exitCode}): ${result.stderr.toString().slice(-2048)}`,
    )
  return result.stdout.toString().trim()
}
const docker = (...args: string[]) => command(['docker', ...args])
const sql = (container: string, statement: string, db = 'postgres') =>
  docker(
    'exec',
    container,
    'psql',
    '-XqAt',
    '-v',
    'ON_ERROR_STOP=1',
    '-U',
    'postgres',
    '-d',
    db,
    '-c',
    statement,
  )

export async function startupTail(path: string, password: string) {
  const log = Bun.file(path)
  const offset = Math.max(0, log.size - 8192)
  const lines = (await log.slice(offset).text()).trimEnd().split('\n')
  if (offset) lines.shift()
  return lines
    .slice(-30)
    .map((line) =>
      /secret|token|password|credential|authorization|api.?key|private.?key|:\/\/[^/ ]+@|tk_/i.test(line)
        ? '[redacted]'
        : line.replaceAll(password, '[redacted]'),
    )
    .join('\n')
}

class Server {
  private cookies = new Map<string, string>()
  constructor(private address: string) {}

  api(path: string, body: undefined, method: 'DELETE'): Promise<void>
  api<T>(path: string, body?: unknown): Promise<T>
  async api<T>(
    path: string,
    body?: unknown,
    method = body === undefined ? 'GET' : 'POST',
  ): Promise<T | void> {
    const response = await fetch(`http://${this.address}/api${path}`, {
      method,
      signal: AbortSignal.timeout(180_000),
      headers: { 'Content-Type': 'application/json', Cookie: [...this.cookies.values()].join('; ') },
      body: JSON.stringify(body),
    })
    for (const header of response.headers.getSetCookie()) {
      const [pair] = header.split(';')
      this.cookies.set(pair.split('=')[0], pair)
    }
    const payload = await response.text()
    if (!response.ok) {
      const problem = JSON.parse(payload)
      throw new Error(`${method} ${path}: HTTP ${response.status}: ${problem.detail ?? problem.title}`)
    }
    if (method === 'DELETE') return
    return JSON.parse(payload)
  }

  async completed<T>(path: string, field: 'state' | 'status') {
    return pollUntil(
      () => this.api<T & Record<typeof field, string> & { error_message?: string }>(path),
      (row) => {
        if (['failed', 'cancelled', 'rolled_back'].includes(row[field])) {
          throw new Error(`${path}: ${row[field]}: ${row.error_message}`)
        }
        return row[field] === 'completed'
      },
      { timeoutMs: 420_000, intervalMs: 1000, label: path },
    )
  }
}

export async function postgresCrossServerRecoveryScenario() {
  try {
    docker('info', '--format', '{{.ServerVersion}}')
  } catch {
    console.log('Skipping cross-server PostgreSQL recovery: Docker unavailable')
    return
  }
  if (!process.env.TEMPS_BIN) throw new Error('Set TEMPS_BIN to the Temps binary under test')
  const binary = resolve(process.env.TEMPS_BIN)
  const password = randomBytes(24).toString('hex') + 'Aa1'
  const runId = randomBytes(6).toString('hex')
  const scratch = mkdtempSync(join(tmpdir(), 'temps-recovery-'))
  const cleanup: Cleanup[] = [() => rmSync(scratch, { recursive: true, force: true })]

  function container(port: number, image: string, env: Record<string, string>, ...args: string[]) {
    const id = command(
      [
        'docker',
        'run',
        '-d',
        '--name',
        `recovery-${runId}-${port}`,
        '-p',
        `127.0.0.1::${port}`,
        ...Object.keys(env).flatMap((key) => ['-e', key]),
        image,
        ...args,
      ],
      { ...process.env, ...env },
    )
    cleanup.push(() => {
      docker('rm', '-fv', id)
    })
    return { id, address: docker('port', id, `${port}/tcp`).split('\n')[0] }
  }

  async function startServer(label: string, catalog: ReturnType<typeof container>) {
    const db = `server_${label}`
    docker('exec', catalog.id, 'createdb', '-U', 'postgres', db)
    const dir = join(scratch, db)
    mkdirSync(dir)
    copyFileSync(
      resolve(import.meta.dir, '../../../../crates/temps-cli/GeoLite2-City.mmdb'),
      join(dir, 'GeoLite2-City.mmdb'),
    )
    const url = `postgresql://postgres:${password}@${catalog.address}/${db}`
    const env = {
      ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TEMPS_'))),
      TEMPS_DATA_DIR: dir,
      DATABASE_URL: url,
      TEMPS_DATABASE_URL: url,
      TEMPS_TELEMETRY: '0',
    }
    const email = 'operator@example.invalid'
    command(
      [
        binary,
        'setup',
        '--non-interactive',
        '--admin-email',
        email,
        '--wildcard-domain',
        '*.example.invalid',
        '--server-ip',
        '127.0.0.1',
        '--skip-ssl',
        '--skip-dns-records',
        '--skip-geolite2-download',
        '--skip-git',
      ],
      { ...env, TEMPS_ADMIN_PASSWORD: password },
    )
    const gateway = JSON.stringify({ enabled: false, container_name: `preview-${label}-${runId}` })
    assert.equal(
      sql(
        catalog.id,
        `UPDATE settings SET data = jsonb_set(data::jsonb, '{preview_gateway}', '${gateway}'::jsonb) WHERE id = 1 RETURNING id`,
        db,
      ),
      '1',
    )

    const listener = Bun.listen({ hostname: '127.0.0.1', port: 0, socket: { data() {} } })
    const address = `127.0.0.1:${listener.port}`
    listener.stop(true)
    const log = join(dir, 'startup.log')
    const fd = openSync(log, 'w', 0o600)
    const proc = (() => {
      try {
        return Bun.spawn(
          [
            binary,
            'serve',
            '--role',
            'console',
            '--console-address',
            address,
            '--proxy-address',
            address,
            '--screenshot-provider',
            'noop',
          ],
          { env, stdout: fd, stderr: fd },
        )
      } finally {
        closeSync(fd)
      }
    })()
    cleanup.push(async () => {
      proc.kill('SIGTERM')
      const timer = setTimeout(() => proc.kill('SIGKILL'), 15_000)
      try {
        await proc.exited
      } finally {
        clearTimeout(timer)
      }
    })
    const server = new Server(address)
    try {
      await pollUntil(
        async () => {
          if (proc.exitCode !== null) throw new Error(`${db} exited during startup (${proc.exitCode})`)
          try {
            return (await server.api<{ success: boolean }>('/auth/login', { email, password })).success
          } catch (error) {
            if (
              error instanceof Error &&
              'code' in error &&
              (error.code === 'ConnectionRefused' || error.code === 'ECONNREFUSED')
            )
              return false
            throw error
          }
        },
        Boolean,
        { timeoutMs: 180_000, intervalMs: 1000, label: `${db} login` },
      )
    } catch (error) {
      throw new Error(`${db} startup failed: ${await startupTail(log, password)}`, { cause: error })
    }
    return server
  }

  async function postgres(server: Server, label: string) {
    const name = `service-${label}-${runId}`
    const { id } = await server.api<{ id: number }>('/external-services', {
      name,
      service_type: 'postgres',
      parameters: { username: 'postgres', database: 'postgres', password },
    })
    cleanup.push(() => server.api(`/external-services/${id}`, undefined, 'DELETE'))
    return { id, name: `postgres-${name}` }
  }

  const errors: unknown[] = []
  try {
    const catalog = container(5432, 'timescale/timescaledb-ha:pg18', { POSTGRES_PASSWORD: password })
    const store = container(
      9000,
      'cgr.dev/chainguard/minio@sha256:f2e56aceb4d38ad9b05c901270946e2353b19f86ba4396f25cf75b15b455b38c',
      { MINIO_ROOT_USER: 'test-access', MINIO_ROOT_PASSWORD: password },
      'server',
      '/tmp/backups',
    )
    await pollUntil(
      async () =>
        Bun.spawnSync(['docker', 'exec', catalog.id, 'pg_isready', '-h', '127.0.0.1'], { timeout: 10_000 })
          .success,
      Boolean,
      { timeoutMs: 180_000, intervalMs: 1000, label: 'catalog PostgreSQL' },
    )
    const source = await startServer('a', catalog)
    const destination = await startServer('b', catalog)
    const origin = await postgres(source, 'a')
    const target = await postgres(destination, 'b')
    const networks = (id: string) =>
      Object.keys(JSON.parse(docker('inspect', '--format', '{{json .NetworkSettings.Networks}}', id)))
    const storeNetworks = networks(store.id)
    for (const network of networks(origin.name)) {
      if (!storeNetworks.includes(network)) docker('network', 'connect', network, store.id)
    }
    const settings = {
      name: 'shared',
      bucket_name: 'backups-bucket',
      bucket_path: '',
      endpoint: `http://${store.address}`,
      region: 'us-east-1',
      access_key_id: 'test-access',
      secret_key: password,
      force_path_style: true,
    }
    const writer = await source.api<{ id: number }>('/backups/s3-sources', settings)
    const reader = await destination.api<{ id: number }>('/backups/s3-sources', settings)
    const listing = `/backups/s3-sources/${reader.id}/backups`
    async function backup() {
      const row = await source.api<{ backup_id: string }>(
        `/backups/external-services/${origin.id}/run`,
        confirmedFullBackup(writer.id),
      )
      await source.completed(`/backups/${row.backup_id}`, 'state')
    }
    async function snapshots() {
      const result = await destination.api<Listing>(listing + '?include_s3_scan=true')
      assert.deepEqual(result.scan_errors, [])
      return result.backups
        .filter((b) => b.format === 'walg' && b.source === 's3_scan')
        .map((b) => b.location)
    }

    sql(
      origin.name,
      `CREATE EXTENSION IF NOT EXISTS pgcrypto;
      CREATE TABLE recovery_probe AS SELECT id, gen_random_bytes(1024) AS payload FROM generate_series(1, 512000) id`,
    )
    const fingerprint =
      "SELECT count(*), sum(octet_length(payload)), md5(string_agg(md5(payload), '' ORDER BY id)) FROM recovery_probe"
    const expected = sql(origin.name, fingerprint)
    assert.deepEqual(expected.split('|').slice(0, 2), ['512000', '524288000'])
    console.log('Backing up 500 MiB on server A')
    await backup()
    assert.deepEqual((await destination.api<Listing>(listing)).backups, [])
    const first = await snapshots()
    assert.equal(first.length, 1)
    const [selected] = first
    assert.match(selected, /_backup_stop_sentinel\.json$/)
    const tracked = await source.api<Listing>(`/backups/s3-sources/${writer.id}/backups`)
    assert.ok(tracked.backups.some((b) => b.location === selected))

    sql(origin.name, 'CREATE TABLE newer_marker AS SELECT 1 AS id')
    await backup()
    const available = await snapshots()
    assert.equal(new Set(available).size, 2)
    assert.ok(available.includes(selected))
    console.log('Restoring the older snapshot on server B')
    const run = await destination.api<{ id: number }>(`/external-services/${target.id}/restore`, {
      mode: 'in_place',
      s3_source_id: reader.id,
      backup_location: selected,
      backup_engine: 'postgres',
      confirm_cross_service: true,
    })
    const restored = await destination.completed<{
      source_backup_id: number | null
      source_backup: { location: string; s3_source_id: number }
    }>(`/restore-runs/${run.id}`, 'status')
    assert.equal(restored.source_backup_id, null)
    assert.equal(restored.source_backup.location, selected)
    assert.equal(restored.source_backup.s3_source_id, reader.id)
    assert.equal(sql(target.name, fingerprint), expected)
    assert.equal(sql(target.name, "SELECT to_regclass('public.newer_marker') IS NULL"), 't')
    console.log('Verified the restored row count, byte count and checksum')
  } catch (error) {
    errors.push(error)
  } finally {
    for (const remove of cleanup.reverse()) {
      try {
        await remove()
      } catch (error) {
        errors.push(error)
      }
    }
  }
  if (errors.length === 1) throw errors[0]
  if (errors.length) throw new AggregateError(errors, 'Cross-server recovery or cleanup failed')
}
