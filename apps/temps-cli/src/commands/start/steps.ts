// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Side-effectful steps for `temps start` / `temps stop`: Docker, downloads,
 * the background server process and the state file. Node APIs only, because
 * the published CLI runs on Node.
 */

import { spawn } from 'node:child_process'
import { createHash, randomBytes } from 'node:crypto'
import { createReadStream, createWriteStream, existsSync, fstatSync, openSync, closeSync, readSync, statSync } from 'node:fs'
import { chmod, cp, mkdir, readdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { basename, dirname, join } from 'node:path'
import { Readable } from 'node:stream'
import { pipeline } from 'node:stream/promises'
import { fileURLToPath } from 'node:url'
import {
  BUNDLED_SKILL,
  DB_CONTAINER,
  DB_IMAGE,
  DB_NAME,
  DB_USER,
  DB_VOLUME,
  DOCKER_NETWORK,
  LOCAL_DOMAIN,
  RELEASE_REPO,
  StartError,
  binaryPath,
  databaseUrl,
  isLocalServerCommand,
  isLocalState,
  localUrls,
  parseChecksum,
  parseJsonOutput,
  parseLocalServerPids,
  pickRelease,
  releaseAssetUrl,
  skillTargets,
  type Channel,
  type LocalPaths,
  type LocalState,
  type ReleaseSummary,
  type WorkloadContainer,
  type WorkloadVolume,
  volumeOwnership,
} from './runtime.js'

export interface RunResult {
  code: number | null
  stdout: string
  stderr: string
}

/** Most output kept from a child process: enough for any result, bounded for a runaway one. */
const MAX_CAPTURED_OUTPUT = 1024 * 1024

function appendCapped(text: string, chunk: Buffer): string {
  const next = text + chunk.toString()
  return next.length > MAX_CAPTURED_OUTPUT ? next.slice(-MAX_CAPTURED_OUTPUT) : next
}

/**
 * Run a command to completion. Secrets go in `env`, never in `args`. Keeps
 * at most the last `MAX_CAPTURED_OUTPUT` characters of each stream.
 */
export function run(
  command: string,
  args: readonly string[],
  options: { env?: NodeJS.ProcessEnv; timeoutMs?: number } = {},
): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      env: { ...process.env, ...options.env },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk: Buffer) => (stdout = appendCapped(stdout, chunk)))
    child.stderr.on('data', (chunk: Buffer) => (stderr = appendCapped(stderr, chunk)))
    const timer = options.timeoutMs
      ? setTimeout(() => child.kill('SIGTERM'), options.timeoutMs)
      : undefined
    child.on('error', (error) => {
      if (timer) clearTimeout(timer)
      reject(error)
    })
    child.on('close', (code) => {
      if (timer) clearTimeout(timer)
      resolve({ code, stdout, stderr })
    })
  })
}

function lastLines(text: string, count = 8): string {
  return text.trim().split('\n').slice(-count).join('\n')
}

// ── State ────────────────────────────────────────────────────────────────

export async function readState(paths: LocalPaths): Promise<LocalState | null> {
  if (!existsSync(paths.state)) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(await readFile(paths.state, 'utf-8'))
  } catch (error) {
    throw new StartError(
      'state',
      'bad_state',
      `Could not read ${paths.state}: ${(error as Error).message}. Run \`temps stop --delete-data\` to start fresh.`,
    )
  }
  if (!isLocalState(parsed)) {
    throw new StartError(
      'state',
      'bad_state',
      `${paths.state} is not a valid state file. Run \`temps stop --delete-data\` to start fresh.`,
    )
  }
  return parsed
}

/** The state file holds the database and admin passwords: owner-only. */
export async function writeState(paths: LocalPaths, state: LocalState): Promise<void> {
  await mkdir(paths.root, { recursive: true, mode: 0o700 })
  const tmp = `${paths.state}.tmp`
  await writeFile(tmp, JSON.stringify(state, null, 2) + '\n', { mode: 0o600 })
  await rename(tmp, paths.state)
}

// ── Lock ─────────────────────────────────────────────────────────────────

interface LockEntry {
  pid: number
  command: string
  startedAt: number
  /** `ps -o lstart=` for `pid` when it took the lock; null where `ps` cannot say. */
  processStart: string | null
}

const LOCK_ATTEMPTS = 4

/** When a process started, as `ps` prints it. Null if it is not running or `ps` cannot say. */
async function processStartTime(pid: number): Promise<string | null> {
  try {
    const ps = await run('ps', ['-o', 'lstart=', '-p', String(pid)], { timeoutMs: 5_000 })
    return ps.code === 0 && ps.stdout.trim() ? ps.stdout.trim() : null
  } catch {
    return null
  }
}

/**
 * Whether the process that wrote a lock entry is still running. A pid alone
 * is not enough: it can be reused by another program after a crash, so the
 * start time recorded with the entry must still be that pid's start time.
 * Every doubt (no recorded start time, `ps` unavailable) counts as running:
 * a lock is only ever disregarded when its writer is certainly gone.
 */
async function writerAlive(entry: LockEntry): Promise<boolean> {
  if (!processAlive(entry.pid)) return false
  if (entry.processStart === null) return true
  const current = await processStartTime(entry.pid)
  return current === null || current === entry.processStart
}

function parseLockEntry(raw: string): LockEntry | null {
  try {
    const entry = JSON.parse(raw) as Partial<LockEntry>
    return typeof entry.pid === 'number' && typeof entry.command === 'string' && typeof entry.startedAt === 'number'
      ? { pid: entry.pid, command: entry.command, startedAt: entry.startedAt, processStart: entry.processStart ?? null }
      : null
  } catch {
    return null
  }
}

/**
 * Entries in the lock directory, other than `own`, whose writer is still
 * running. Removes the ones left by a process that is certainly gone, which
 * is safe for exactly that reason; a live entry is never touched.
 */
async function liveLockEntries(paths: LocalPaths, own: string): Promise<LockEntry[]> {
  const live: LockEntry[] = []
  for (const name of await readdir(paths.lock)) {
    if (name === own) continue
    const path = join(paths.lock, name)
    if (!name.endsWith('.json')) {
      // A half-written entry (`<pid>-<nonce>.tmp`) is not a claim yet; it is
      // only litter once its writer is gone.
      const pid = Number.parseInt(name, 10)
      if (!processAlive(Number.isNaN(pid) ? undefined : pid)) await rm(path, { force: true })
      continue
    }
    const raw = await readFile(path, 'utf-8').catch(() => null)
    if (raw === null) continue // released while we were listing
    const entry = parseLockEntry(raw)
    // Entries appear by rename, complete; one that does not parse was not written by this code.
    if (entry && (await writerAlive(entry))) live.push(entry)
    else await rm(path, { force: true })
  }
  return live
}

/**
 * Take the lock that serializes `temps start` and `temps stop`. Two of them
 * at once would each generate credentials, write `state.json` and unpack the
 * binary, and the loser's password would not match the database the winner
 * created.
 *
 * The lock is a directory of claims, one file per process, and the protocol
 * never removes another live process's claim, so there is no takeover step
 * to race on:
 *
 * 1. Publish a claim: write it under a temporary name, then rename it into
 *    place, so it appears whole and at one instant.
 * 2. List the directory. If no other live claim exists, the lock is held.
 * 3. Otherwise withdraw the claim and retry after a random pause; give up
 *    after a few rounds with an error naming the holder.
 *
 * Why two commands cannot both hold it: each lists only after its own claim
 * is published, so whichever published second must see the first one's
 * claim, unless the first has already withdrawn or released it. Two that
 * publish together may both withdraw; the random pause separates them on the
 * next round. A claim left by a crash or Ctrl-C is skipped and removed once
 * its writer is certainly gone (see `writerAlive`), and that is the only
 * time anything but a command's own claim is deleted.
 *
 * Fails rather than queues behind a running command, which may be waiting
 * on a prompt. Returns the release function.
 */
export async function acquireLock(paths: LocalPaths, command: string): Promise<() => Promise<void>> {
  await mkdir(paths.lock, { recursive: true, mode: 0o700 })
  const id = `${process.pid}-${randomBytes(8).toString('hex')}`
  const claim = join(paths.lock, `${id}.json`)
  const draft = join(paths.lock, `${id}.tmp`)
  const entry: LockEntry = {
    pid: process.pid,
    command,
    startedAt: Date.now(),
    processStart: await processStartTime(process.pid),
  }
  for (let attempt = 1; ; attempt++) {
    await writeFile(draft, JSON.stringify(entry), { mode: 0o600 })
    await rename(draft, claim)
    let others: LockEntry[]
    try {
      others = await liveLockEntries(paths, `${id}.json`)
    } catch (error) {
      await rm(claim, { force: true })
      throw error
    }
    if (others.length === 0) return () => rm(claim, { force: true })
    await rm(claim, { force: true })
    if (attempt >= LOCK_ATTEMPTS) {
      const holder = others[0] as LockEntry
      throw new StartError(
        'lock',
        'busy',
        `\`temps ${holder.command}\` (pid ${holder.pid}) is already working on your local Temps. ` +
          'Wait for it to finish, then run this again.',
      )
    }
    await new Promise((resolve) => setTimeout(resolve, 50 + Math.random() * 200))
  }
}

export async function withLock<T>(paths: LocalPaths, command: string, work: () => Promise<T>): Promise<T> {
  const release = await acquireLock(paths, command)
  try {
    return await work()
  } finally {
    await release()
  }
}

// ── Preflight ────────────────────────────────────────────────────────────

export async function dockerVersion(): Promise<string> {
  let result: RunResult
  try {
    result = await run('docker', ['info', '--format', '{{.ServerVersion}}'], { timeoutMs: 15_000 })
  } catch {
    throw new StartError(
      'preflight',
      'docker_missing',
      'Docker is not installed. Install Docker Desktop, OrbStack or Colima, start it, and run `temps start` again.',
    )
  }
  if (result.code !== 0) {
    throw new StartError(
      'preflight',
      'docker_not_running',
      `Docker is installed but not running (${lastLines(result.stderr, 2) || 'no response'}). Start Docker Desktop, OrbStack or Colima, then run \`temps start\` again.`,
    )
  }
  return result.stdout.trim()
}

export function isPortFree(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const server = createServer()
    server.once('error', () => resolve(false))
    server.listen(port, '127.0.0.1', () => server.close(() => resolve(true)))
  })
}

export async function pickPort(
  candidates: readonly number[],
  taken: readonly number[],
  label: string,
): Promise<number> {
  for (const port of candidates) {
    if (taken.includes(port)) continue
    if (await isPortFree(port)) return port
  }
  throw new StartError(
    'preflight',
    'no_free_port',
    `No free port for the ${label} (tried ${candidates[0]}–${candidates[candidates.length - 1]} on 127.0.0.1).`,
  )
}

// ── Server binary ────────────────────────────────────────────────────────

async function fetchOk(url: string, timeoutMs: number): Promise<Response> {
  const headers: Record<string, string> = { 'User-Agent': 'temps-cli' }
  if (url.startsWith('https://api.github.com/') && process.env.GITHUB_TOKEN) {
    headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`
  }
  let response: Response
  try {
    response = await fetch(url, { headers, signal: AbortSignal.timeout(timeoutMs), redirect: 'follow' })
  } catch (error) {
    throw new StartError('download', 'network', `Could not reach ${url}: ${(error as Error).message}`)
  }
  if (!response.ok) {
    throw new StartError('download', 'http_error', `GET ${url} returned HTTP ${response.status}.`)
  }
  return response
}

/** Newest release on `channel` that has an archive for this machine. */
export async function resolveVersion(channel: Channel, target: string): Promise<string> {
  const releases: ReleaseSummary[] = []
  if (channel === 'stable') {
    const latest = (await (await fetchOk(`https://api.github.com/repos/${RELEASE_REPO}/releases/latest`, 15_000)).json()) as ReleaseSummary
    releases.push(latest)
  } else {
    for (let page = 1; page <= 5; page++) {
      const batch = (await (
        await fetchOk(`https://api.github.com/repos/${RELEASE_REPO}/releases?per_page=100&page=${page}`, 15_000)
      ).json()) as ReleaseSummary[]
      releases.push(...batch)
      if (batch.length < 100 || pickRelease(releases, channel, target)) break
    }
  }
  const version = pickRelease(releases, channel, target)
  if (!version) {
    throw new StartError(
      'download',
      'no_release',
      `No ${channel} release of Temps has a ${target} build. Try --channel nightly, or pin one with --version.`,
    )
  }
  return version
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash('sha256')
  await pipeline(createReadStream(path), hash)
  return hash.digest('hex')
}

/**
 * Download, verify and unpack the server binary for `version`. A version
 * already on disk is reused, so later starts need no network.
 */
export async function ensureBinary(
  paths: LocalPaths,
  version: string,
  target: string,
): Promise<{ path: string; downloaded: boolean; bytes: number }> {
  const bin = binaryPath(paths, version)
  if (existsSync(bin)) return { path: bin, downloaded: false, bytes: 0 }

  const dir = dirname(bin)
  const staging = `${dir}.partial`
  await rm(staging, { recursive: true, force: true })
  await mkdir(staging, { recursive: true, mode: 0o700 })
  const archive = join(staging, `${target}.tar.gz`)
  try {
    const expected = parseChecksum(
      await (await fetchOk(releaseAssetUrl(version, target, '.tar.gz.sha256'), 30_000)).text(),
    )
    const response = await fetchOk(releaseAssetUrl(version, target), 10 * 60_000)
    if (!response.body) {
      throw new StartError('download', 'empty_body', `The ${target} archive for ${version} came back empty.`)
    }
    await pipeline(Readable.fromWeb(response.body as never), createWriteStream(archive, { mode: 0o600 }))
    const actual = await sha256File(archive)
    if (actual !== expected) {
      throw new StartError(
        'download',
        'checksum_mismatch',
        `The ${target} archive for ${version} failed its checksum (expected ${expected}, got ${actual}). Nothing was installed.`,
      )
    }
    const untar = await run('tar', ['-xzf', archive, '-C', staging], { timeoutMs: 120_000 })
    if (untar.code !== 0) {
      throw new StartError('download', 'extract_failed', `Could not unpack ${archive}: ${lastLines(untar.stderr, 3)}`)
    }
    await rm(archive, { force: true })
    if (!existsSync(join(staging, 'temps'))) {
      throw new StartError('download', 'no_binary', `The ${target} archive for ${version} has no \`temps\` binary.`)
    }
    await chmod(join(staging, 'temps'), 0o755)
    await rm(dir, { recursive: true, force: true })
    await rename(staging, dir)
    return { path: bin, downloaded: true, bytes: statSync(bin).size }
  } catch (error) {
    await rm(staging, { recursive: true, force: true })
    throw error
  }
}

// ── Database ─────────────────────────────────────────────────────────────

async function docker(args: readonly string[], options: { env?: NodeJS.ProcessEnv; timeoutMs?: number } = {}): Promise<RunResult> {
  return run('docker', args, { timeoutMs: 60_000, ...options })
}

export async function containerState(name: string): Promise<'missing' | 'running' | 'stopped'> {
  const result = await docker(['inspect', '--format', '{{.State.Running}}', name], { timeoutMs: 15_000 })
  if (result.code !== 0) return 'missing'
  return result.stdout.trim() === 'true' ? 'running' : 'stopped'
}

/**
 * Start the local database, creating it on first run. The password reaches
 * `docker run` through the environment (`-e NAME` with no value), so it
 * never appears in a process listing.
 */
export async function ensureDatabase(state: LocalState): Promise<{ created: boolean; pulled: boolean }> {
  const current = await containerState(DB_CONTAINER)
  let created = false
  let pulled = false
  if (current === 'stopped') {
    const started = await docker(['start', DB_CONTAINER])
    if (started.code !== 0) {
      throw new StartError('database', 'start_failed', `Could not start ${DB_CONTAINER}: ${lastLines(started.stderr, 3)}`)
    }
  } else if (current === 'missing') {
    const image = await docker(['image', 'inspect', DB_IMAGE], { timeoutMs: 15_000 })
    if (image.code !== 0) {
      const pull = await docker(['pull', '--quiet', DB_IMAGE], { timeoutMs: 20 * 60_000 })
      if (pull.code !== 0) {
        throw new StartError('database', 'pull_failed', `Could not pull ${DB_IMAGE}: ${lastLines(pull.stderr, 3)}`)
      }
      pulled = true
    }
    const result = await docker(
      [
        'run', '-d',
        '--name', DB_CONTAINER,
        '--label', 'sh.temps.local=database',
        '--restart', 'unless-stopped',
        '-e', `POSTGRES_USER=${DB_USER}`,
        '-e', `POSTGRES_DB=${DB_NAME}`,
        '-e', 'POSTGRES_PASSWORD',
        '-v', `${DB_VOLUME}:/var/lib/postgresql`,
        '-p', `127.0.0.1:${state.dbPort}:5432`,
        DB_IMAGE,
      ],
      { env: { POSTGRES_PASSWORD: state.dbPassword } },
    )
    if (result.code !== 0) {
      throw new StartError('database', 'run_failed', `Could not create ${DB_CONTAINER}: ${lastLines(result.stderr, 3)}`)
    }
    created = true
  }

  const deadline = Date.now() + 90_000
  while (Date.now() < deadline) {
    const ready = await docker(['exec', DB_CONTAINER, 'pg_isready', '-U', DB_USER, '-h', '127.0.0.1'], { timeoutMs: 10_000 })
    if (ready.code === 0) return { created, pulled }
    await new Promise((resolve) => setTimeout(resolve, 250))
  }
  const logs = await docker(['logs', '--tail', '20', DB_CONTAINER])
  throw new StartError(
    'database',
    'not_ready',
    `${DB_CONTAINER} did not accept connections within 90s. Last log lines:\n${lastLines(logs.stdout + logs.stderr)}`,
  )
}

// ── Platform setup and API key ───────────────────────────────────────────

/**
 * Environment for every invocation of the server binary. The network name
 * scopes this install's containers (see `DOCKER_NETWORK`).
 */
function serverEnv(state: LocalState, paths: LocalPaths): NodeJS.ProcessEnv {
  return {
    TEMPS_DATABASE_URL: databaseUrl(state),
    TEMPS_DATA_DIR: paths.data,
    TEMPS_NETWORK_NAME: DOCKER_NETWORK,
  }
}

/**
 * First run only: create the admin and point the platform at the local
 * wildcard domain. `temps setup --auto` also runs the database migrations.
 */
export async function runSetup(bin: string, state: LocalState, paths: LocalPaths): Promise<void> {
  await mkdir(paths.data, { recursive: true, mode: 0o700 })
  const result = await run(
    bin,
    [
      'setup',
      '--auto',
      '--admin-email', state.adminEmail,
      '--wildcard-domain', `*.${LOCAL_DOMAIN}`,
      '--server-ip', '127.0.0.1',
      '--external-url', localUrls(state.proxyPort).external,
      '--output-format', 'json',
    ],
    {
      env: { ...serverEnv(state, paths), TEMPS_ADMIN_PASSWORD: state.adminPassword },
      timeoutMs: 5 * 60_000,
    },
  )
  if (result.code !== 0) {
    throw new StartError(
      'setup',
      'setup_failed',
      `\`temps setup\` exited with ${result.code}:\n${lastLines(result.stderr || result.stdout)}`,
    )
  }
  const output = parseJsonOutput<{ success?: boolean; message?: string }>(result.stdout, 'setup')
  if (output.success !== true) {
    throw new StartError('setup', 'setup_failed', `\`temps setup\` reported failure: ${output.message ?? 'no message'}`)
  }
}

export interface MintedKey {
  apiKey: string
  keyPrefix: string
}

/**
 * Mint an admin API key for this machine's CLI and agents. Its value travels
 * only through this process's pipes and lands in the CLI's 0600 context file.
 */
export async function mintApiKey(bin: string, state: LocalState, paths: LocalPaths, name: string): Promise<MintedKey> {
  const result = await run(
    bin,
    // No --email: older releases lack it, and the local admin is the only user.
    ['api-key', '--name', name, '--role', 'admin', '--output-format', 'json'],
    { env: serverEnv(state, paths), timeoutMs: 60_000 },
  )
  if (result.code !== 0) {
    throw new StartError('api_key', 'mint_failed', `\`temps api-key\` exited with ${result.code}:\n${lastLines(result.stderr || result.stdout)}`)
  }
  const output = parseJsonOutput<{ api_key?: string; key_prefix?: string }>(result.stdout, 'api-key')
  if (!output.api_key) {
    throw new StartError('api_key', 'mint_failed', '`temps api-key` returned no key.')
  }
  return { apiKey: output.api_key, keyPrefix: output.key_prefix ?? output.api_key.slice(0, 8) }
}

// ── Server process ───────────────────────────────────────────────────────

export function processAlive(pid: number | undefined): boolean {
  if (!pid) return false
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

/** Start `temps serve` in the background, logging to `server.log`. */
export async function startServer(bin: string, state: LocalState, paths: LocalPaths): Promise<number> {
  await mkdir(paths.root, { recursive: true, mode: 0o700 })
  const log = openSync(paths.log, 'a', 0o600)
  try {
    const child = spawn(
      bin,
      ['serve', '--address', `127.0.0.1:${state.proxyPort}`, '--console-address', `127.0.0.1:${state.consolePort}`],
      {
        detached: true,
        stdio: ['ignore', log, log],
        env: { ...process.env, ...serverEnv(state, paths) },
      },
    )
    if (!child.pid) {
      throw new StartError('server', 'spawn_failed', `Could not start ${bin} serve.`)
    }
    child.unref()
    return child.pid
  } finally {
    closeSync(log)
  }
}

/** How much of the end of a log is read to show its last lines. */
const TAIL_BYTES = 64 * 1024

/**
 * The last `count` lines of a log, reading at most `TAIL_BYTES` from its
 * end: `server.log` grows across restarts and must never be loaded whole
 * just to report an error.
 */
export function tailFile(path: string, count = 15): string {
  let fd: number | undefined
  try {
    fd = openSync(path, 'r')
    const size = fstatSync(fd).size
    const length = Math.min(size, TAIL_BYTES)
    const buffer = Buffer.alloc(length)
    const read = readSync(fd, buffer, 0, length, size - length)
    let text = buffer.subarray(0, read).toString('utf-8')
    // A window that starts mid-file starts mid-line: drop the partial line.
    if (size > length) text = text.slice(text.indexOf('\n') + 1)
    return lastLines(text.replace(/\x1b\[[0-9;]*m/g, ''), count) || '(log is empty)'
  } catch {
    return '(no log yet)'
  } finally {
    if (fd !== undefined) closeSync(fd)
  }
}

export async function serverHealthy(consolePort: number): Promise<boolean> {
  try {
    const response = await fetch(`http://127.0.0.1:${consolePort}/health`, { signal: AbortSignal.timeout(1_000) })
    return response.ok
  } catch {
    return false
  }
}

export async function waitForServer(state: LocalState, paths: LocalPaths, pid: number, timeoutMs = 120_000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await serverHealthy(state.consolePort)) return
    if (!processAlive(pid)) {
      throw new StartError(
        'server',
        'exited',
        `Temps exited during startup. Last lines of ${paths.log}:\n${tailFile(paths.log)}`,
      )
    }
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  throw new StartError(
    'server',
    'not_ready',
    `Temps did not become healthy within ${timeoutMs / 1000}s. Last lines of ${paths.log}:\n${tailFile(paths.log)}`,
  )
}

/** Whether `pid` is alive and is the server `temps start` launched. */
export async function isLocalServer(pid: number | undefined, paths: LocalPaths): Promise<boolean> {
  if (!processAlive(pid)) return false
  try {
    const ps = await run('ps', ['-o', 'command=', '-p', String(pid)], { timeoutMs: 5_000 })
    return ps.code === 0 && isLocalServerCommand(ps.stdout, paths)
  } catch {
    return false
  }
}

/**
 * SIGTERM, then SIGKILL after 15s. Returns false if it was not running, or
 * if the saved pid now belongs to another program, which is left alone.
 */
export async function stopServer(pid: number | undefined, paths: LocalPaths): Promise<boolean> {
  if (!(await isLocalServer(pid, paths))) return false
  process.kill(pid as number, 'SIGTERM')
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline) {
    if (!processAlive(pid)) return true
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  process.kill(pid as number, 'SIGKILL')
  return true
}

/**
 * Pids of every running local server, from the process table. `null` when
 * `ps` cannot list processes here (the caller falls back to the saved pid).
 */
export async function localServerPids(paths: LocalPaths): Promise<number[] | null> {
  try {
    const ps = await run('ps', ['-axo', 'pid=,command='], { timeoutMs: 10_000 })
    return ps.code === 0 ? parseLocalServerPids(ps.stdout, paths) : null
  } catch {
    return null
  }
}

/**
 * Stop every local server process. Works from the process table, so it does
 * not need a readable state file; `savedPid` is only a fallback where `ps`
 * is unavailable, and is verified before it is signalled either way.
 */
export async function stopServers(paths: LocalPaths, savedPid: number | undefined): Promise<boolean> {
  const pids = (await localServerPids(paths)) ?? (savedPid === undefined ? [] : [savedPid])
  let stopped = false
  for (const pid of pids) stopped = (await stopServer(pid, paths)) || stopped
  return stopped
}

export async function stopDatabase(): Promise<boolean> {
  if ((await containerState(DB_CONTAINER)) !== 'running') return false
  const result = await docker(['stop', DB_CONTAINER])
  if (result.code !== 0) {
    throw new StartError('database', 'stop_failed', `Could not stop ${DB_CONTAINER}: ${lastLines(result.stderr, 3)}`)
  }
  return true
}

export interface RemovedWorkloads {
  containers: string[]
  volumes: string[]
  /** Volumes or the network left in place, each with its reason. */
  kept: string[]
}

function parseLabels(json: string | undefined): Record<string, string> {
  try {
    return (JSON.parse(json ?? 'null') as Record<string, string> | null) ?? {}
  } catch {
    return {}
  }
}

/**
 * Remove what this install deployed: every container on `network` (apps,
 * Compose stacks, managed services), the volumes this install created for
 * them, and the network. Once the platform database is gone nothing else
 * can manage them, so they must go with it.
 *
 * Containers are scoped by network membership, which no other Temps on
 * this Docker engine shares. Volumes need more than that: a container can
 * mount a volume that belongs to something else, so a volume is deleted
 * only with proof that this install created it (`volumeOwnership`), and
 * Docker itself refuses one another container still uses. Every volume
 * kept is reported with its reason; nothing is forced.
 */
export async function removeWorkloads(
  network: string = DOCKER_NETWORK,
  installVolume: string = DB_VOLUME,
): Promise<RemovedWorkloads> {
  const removed: RemovedWorkloads = { containers: [], volumes: [], kept: [] }
  const listed = await docker(['ps', '-aq', '--no-trunc', '--filter', `network=${network}`])
  if (listed.code !== 0) {
    throw new StartError('workloads', 'list_failed', `Could not list containers on ${network}: ${lastLines(listed.stderr, 3)}`)
  }
  const ids = listed.stdout.split('\n').map((line) => line.trim()).filter(Boolean)
  const containers: WorkloadContainer[] = []
  if (ids.length > 0) {
    const format = '{{.Name}}\t{{json .Config.Labels}}\t{{range .Mounts}}{{if eq .Type "volume"}}{{.Name}} {{end}}{{end}}'
    const inspected = await docker(['inspect', '--format', format, ...ids])
    if (inspected.code !== 0) {
      throw new StartError('workloads', 'inspect_failed', `Could not inspect the containers on ${network}: ${lastLines(inspected.stderr, 3)}`)
    }
    for (const line of inspected.stdout.split('\n')) {
      const [name, labels, mounts = ''] = line.split('\t')
      if (!name) continue
      containers.push({ name: name.replace(/^\//, ''), labels: parseLabels(labels), volumes: mounts.split(' ').filter(Boolean) })
    }
    // -v also removes the containers' anonymous volumes.
    const rmContainers = await docker(['rm', '-f', '-v', ...ids], { timeoutMs: 5 * 60_000 })
    if (rmContainers.code !== 0) {
      throw new StartError('workloads', 'rm_failed', `Could not remove this install's containers: ${lastLines(rmContainers.stderr, 3)}`)
    }
    removed.containers = containers.map((container) => container.name)
  }
  const installed = (await inspectVolume(installVolume))?.createdAt
  const installedAt = installed !== undefined && Number.isFinite(installed) ? installed : null
  const volumes = [...new Set(containers.flatMap((container) => container.volumes))].sort()
  for (const name of volumes) {
    const volume = await inspectVolume(name)
    if (volume === null) continue // an anonymous volume `rm -v` already removed
    const mountedBy = containers.filter((container) => container.volumes.includes(name))
    const verdict = volumeOwnership(volume, mountedBy, installedAt)
    if (!verdict.owned) {
      removed.kept.push(`volume ${name}: ${verdict.reason}`)
      continue
    }
    const rmVolume = await docker(['volume', 'rm', name])
    if (rmVolume.code === 0) removed.volumes.push(name)
    else if (!/no such volume/i.test(rmVolume.stderr)) removed.kept.push(`volume ${name}: ${lastLines(rmVolume.stderr, 1)}`)
  }
  const rmNetwork = await docker(['network', 'rm', network])
  if (rmNetwork.code !== 0 && !/not found|no such network/i.test(rmNetwork.stderr)) {
    removed.kept.push(`network ${network}: ${lastLines(rmNetwork.stderr, 1)}`)
  }
  return removed
}

/** A volume's creation time and labels, or null if it does not exist. */
async function inspectVolume(name: string): Promise<WorkloadVolume | null> {
  const inspected = await docker(['volume', 'inspect', '--format', '{{.CreatedAt}}\t{{json .Labels}}', name], { timeoutMs: 15_000 })
  if (inspected.code !== 0) return null
  const [created, labels] = inspected.stdout.trim().split('\t')
  // An unreadable date stays NaN, which `volumeOwnership` treats as "not ours".
  return { name, createdAt: Date.parse(created ?? ''), labels: parseLabels(labels) }
}

/**
 * Destroy this install's sandboxes through its own API, the only place
 * that knows which sandbox containers are its own (they are not on
 * `DOCKER_NETWORK`). Returns the ids destroyed and the ones the server
 * refused, with its reason.
 */
export async function destroySandboxes(apiUrl: string, apiKey: string): Promise<{ destroyed: string[]; failed: string[] }> {
  const headers = { Authorization: `Bearer ${apiKey}` }
  const destroyed: string[] = []
  const failed: string[] = []
  // Each sandbox is tried once. A destroyed sandbox leaves the listing, so
  // after a page that destroyed something the same page is read again; a
  // page that destroyed nothing (all refused) is stepped past. The round
  // limit bounds both the requests and the ids remembered.
  const attempted = new Set<string>()
  let page = 1
  for (let round = 0; round < 200; round++) {
    const response = await fetch(`${apiUrl}/v1/sandboxes?page=${page}&page_size=100`, { headers, signal: AbortSignal.timeout(15_000) })
    if (!response.ok) {
      // Older releases have no sandbox API: nothing to destroy.
      if (response.status === 404) break
      throw new StartError('workloads', 'sandbox_list_failed', `GET ${apiUrl}/v1/sandboxes returned HTTP ${response.status}.`)
    }
    const listed = ((await response.json()) as { sandboxes?: { id: string }[] }).sandboxes ?? []
    if (listed.length === 0) break
    let destroyedHere = 0
    for (const sandbox of listed) {
      if (attempted.has(sandbox.id)) continue
      attempted.add(sandbox.id)
      const result = await fetch(`${apiUrl}/v1/sandboxes/${encodeURIComponent(sandbox.id)}/destroy`, {
        method: 'POST',
        headers,
        signal: AbortSignal.timeout(120_000),
      })
      if (result.ok || result.status === 404) {
        destroyed.push(sandbox.id)
        destroyedHere += 1
      } else {
        failed.push(`${sandbox.id}: HTTP ${result.status}`)
      }
    }
    if (destroyedHere === 0) page += 1
  }
  return { destroyed, failed }
}

/** Names of every sandbox container on this Docker engine, whichever Temps made it. */
export async function sandboxContainers(): Promise<string[]> {
  const listed = await docker(['ps', '-a', '--filter', 'label=sh.temps.sandbox=true', '--format', '{{.Names}}'])
  return listed.code === 0 ? listed.stdout.split('\n').map((line) => line.trim()).filter(Boolean) : []
}

/**
 * Remove the database container, its volume and everything in
 * `~/.temps/local` except the downloaded binaries (so starting fresh does
 * not download Temps again) and the lock the caller holds.
 */
export async function deleteLocalData(paths: LocalPaths): Promise<void> {
  await docker(['rm', '-f', DB_CONTAINER])
  const volume = await docker(['volume', 'rm', DB_VOLUME])
  if (volume.code !== 0 && !/no such volume/i.test(volume.stderr)) {
    throw new StartError('database', 'volume_rm_failed', `Could not remove volume ${DB_VOLUME}: ${lastLines(volume.stderr, 3)}`)
  }
  if (!existsSync(paths.root)) return
  for (const entry of await readdir(paths.root)) {
    if (join(paths.root, entry) === paths.bin || entry.startsWith(basename(paths.lock))) continue
    await rm(join(paths.root, entry), { recursive: true, force: true })
  }
}

// ── Skill ────────────────────────────────────────────────────────────────

/**
 * The folder of the Temps skill. It ships inside the npm package
 * (`dist/skills/temps`); from a source checkout it is the repo's
 * `skills/temps`. Null when this build has neither.
 */
export function findBundledSkill(from: string = fileURLToPath(import.meta.url)): string | null {
  let dir = dirname(from)
  for (let i = 0; i < 7; i++) {
    const candidate = join(dir, 'skills', BUNDLED_SKILL)
    if (existsSync(join(candidate, 'SKILL.md'))) return candidate
    const parent = dirname(dir)
    if (parent === dir) break
    dir = parent
  }
  return null
}

/**
 * Copy the Temps skill into each agent's skills directory under `base`,
 * replacing an older copy of it. Other skills already there are left alone.
 * Returns the directories it was installed into.
 */
export async function installSkill(source: string, base: string): Promise<string[]> {
  const targets = skillTargets(base)
  for (const target of targets) {
    const destination = join(target, BUNDLED_SKILL)
    await mkdir(target, { recursive: true })
    await rm(destination, { recursive: true, force: true })
    await cp(source, destination, { recursive: true })
  }
  return targets
}
