// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for `temps start`: everything here is deterministic given its
 * inputs, so it is unit-tested without Docker, a network or a filesystem.
 * The side-effectful steps live in `steps.ts`.
 */

import { randomBytes } from 'node:crypto'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { CliError } from '../../utils/errors.js'

export const RELEASE_REPO = 'gotempsh/temps'

/**
 * TimescaleDB image for the local database. The slim image (about half the
 * size of `timescaledb-ha`) ships the three extensions Temps needs:
 * `timescaledb`, `vector` and `pg_stat_statements`. Pinned so a fresh start
 * always gets the database Temps was tested against.
 */
export const DB_IMAGE = 'timescale/timescaledb:2.30.2-pg18'
export const DB_CONTAINER = 'temps-local-db'
export const DB_VOLUME = 'temps-local-db-data'
export const DB_USER = 'temps'

/**
 * Docker network the local server puts every app, Compose stack and managed
 * service on (`TEMPS_NETWORK_NAME`). A name of its own is what lets
 * `temps stop --delete-data` find exactly this install's containers: labels
 * such as `sh.temps.managed` are shared by every Temps on the same Docker
 * engine, membership of this network is not.
 */
export const DOCKER_NETWORK = 'temps-local'
export const DB_NAME = 'temps'

/** Context name the CLI (and every agent using the skills) talks to. */
export const LOCAL_CONTEXT = 'local'
export const LOCAL_ADMIN_EMAIL = 'admin@localhost'

/** Wildcard domain that resolves to 127.0.0.1, so apps get real hostnames. */
export const LOCAL_DOMAIN = '127-0-0-1.sslip.io'

export type Channel = 'stable' | 'beta' | 'nightly'
export const CHANNELS: readonly Channel[] = ['stable', 'beta', 'nightly']

export type SkillsScope = 'project' | 'global' | 'none'

/** What `temps start` remembers between runs, in `~/.temps/local/state.json`. */
export interface LocalState {
  version: string
  dbPort: number
  dbPassword: string
  proxyPort: number
  consolePort: number
  adminEmail: string
  adminPassword: string
  /** Set once `temps setup` has created the admin and configured the domain. */
  setupComplete: boolean
  pid?: number
}

export interface LocalPaths {
  root: string
  state: string
  data: string
  bin: string
  log: string
  lock: string
}

export function localPaths(home: string = homedir()): LocalPaths {
  const root = join(home, '.temps', 'local')
  return {
    root,
    state: join(root, 'state.json'),
    data: join(root, 'data'),
    bin: join(root, 'bin'),
    log: join(root, 'server.log'),
    lock: join(root, 'run.lock'),
  }
}

export function binaryPath(paths: LocalPaths, version: string): string {
  return join(paths.bin, version, 'temps')
}

/**
 * Whether a process's command line is the server `temps start` launched: the
 * binary under `~/.temps/local/bin`, running `serve`. A saved pid outlives a
 * reboot and can be reused by an unrelated program, so `temps stop` checks
 * this before it signals anything.
 */
export function isLocalServerCommand(command: string, paths: LocalPaths): boolean {
  const line = command.trim()
  const prefix = `${paths.bin}/`
  // `<bin>/<version>/temps serve …`; the home folder may contain spaces.
  return line.startsWith(prefix) && /^[^/\s]+\/temps serve(\s|$)/.test(line.slice(prefix.length))
}

/** A container on the install's network, as far as a reset needs to know it. */
export interface WorkloadContainer {
  name: string
  labels: Record<string, string>
  /** Names of the Docker volumes it mounts. */
  volumes: string[]
}

export interface WorkloadVolume {
  name: string
  /** Milliseconds since the epoch. */
  createdAt: number
  labels: Record<string, string>
}

const COMPOSE_PROJECT_LABEL = 'com.docker.compose.project'
/** Labels Temps puts on every managed service container (one per service generation). */
const SERVICE_LABELS = ['temps.service_type', 'temps.service', 'sh.temps.service.type']
/** Labels Temps puts on every app container; a managed service never has them. */
const APP_LABELS = ['sh.temps.project_id', 'sh.temps.deploy_id', COMPOSE_PROJECT_LABEL]

/**
 * A managed service (Postgres, Redis, …): Temps creates the container and
 * chooses its volumes, the user never does. An app container cannot pass
 * for one by shipping a service label in its image, because Temps always
 * adds the app labels on top.
 */
function isManagedService(container: WorkloadContainer): boolean {
  return (
    SERVICE_LABELS.some((label) => label in container.labels) &&
    !APP_LABELS.some((label) => label in container.labels)
  )
}

/**
 * Whether a reset may delete a volume: only with positive proof that this
 * install created it. Being mounted by one of the install's containers is
 * not proof (an app can be pointed at a volume that belongs to something
 * else), and neither is being newer than the install. A volume is owned
 * when it is not older than the install AND either
 *
 * - every container that mounted it is a managed service, or
 * - Docker Compose created it for the stack that mounted it: it carries
 *   the same `com.docker.compose.project` label as every container using
 *   it. A volume declared `external` in a Compose file is not created by
 *   Compose and never has that label.
 *
 * Anything else is kept, with the reason returned for the user.
 */
export function volumeOwnership(
  volume: WorkloadVolume,
  mountedBy: readonly WorkloadContainer[],
  installedAt: number | null,
): { owned: true } | { owned: false; reason: string } {
  if (installedAt === null) {
    return { owned: false, reason: 'the install date is unknown, so it cannot be shown to belong to this install' }
  }
  if (!Number.isFinite(volume.createdAt)) return { owned: false, reason: 'its creation date could not be read' }
  if (volume.createdAt < installedAt) return { owned: false, reason: 'it existed before this install' }
  if (mountedBy.length === 0) return { owned: false, reason: 'no container of this install mounted it' }
  if (mountedBy.every(isManagedService)) return { owned: true }
  const project = volume.labels[COMPOSE_PROJECT_LABEL]
  if (project && mountedBy.every((container) => container.labels[COMPOSE_PROJECT_LABEL] === project)) {
    return { owned: true }
  }
  const users = mountedBy.map((container) => container.name).join(', ')
  return { owned: false, reason: `Temps did not create it (it was attached to ${users})` }
}

/**
 * Pids of every local server in `ps -axo pid=,command=` output. Finds the
 * server from the process table alone, so `temps stop` works when the state
 * file is unreadable and never has to trust a saved pid.
 */
export function parseLocalServerPids(psOutput: string, paths: LocalPaths): number[] {
  const pids: number[] = []
  for (const line of psOutput.split('\n')) {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line)
    if (match && isLocalServerCommand(match[2] as string, paths)) pids.push(Number(match[1]))
  }
  return pids
}

/** A failed step of `temps start` / `temps stop`, with what to do next. */
export class StartError extends CliError {
  constructor(
    public readonly step: string,
    public readonly reason: string,
    message: string,
  ) {
    super(message, `START_${reason.toUpperCase()}`, 1)
    this.name = 'StartError'
  }
}

/** Release asset name for this machine, e.g. `temps-darwin-arm64.tar.gz`. */
export function releaseTarget(platform: string, arch: string): string {
  const os = platform === 'darwin' ? 'darwin' : platform === 'linux' ? 'linux' : null
  const cpu = arch === 'arm64' ? 'arm64' : arch === 'x64' ? 'amd64' : null
  if (!os || !cpu) {
    throw new StartError(
      'preflight',
      'unsupported_platform',
      `temps start supports macOS and Linux on x64 or arm64; this machine is ${platform}/${arch}. ` +
        'Install Temps on a Linux server instead: curl -fsSL https://temps.sh/deploy.sh | bash',
    )
  }
  return `temps-${os}-${cpu}`
}

export function releaseAssetUrl(version: string, target: string, suffix = '.tar.gz'): string {
  return `https://github.com/${RELEASE_REPO}/releases/download/${encodeURIComponent(version)}/${target}${suffix}`
}

export interface ReleaseSummary {
  tag_name: string
  draft?: boolean
  prerelease?: boolean
  assets?: { name: string }[]
}

/**
 * Pick the newest release on a channel, mirroring the installer and
 * `temps upgrade`: stable is the newest non-prerelease, beta the newest
 * non-nightly tag, nightly the newest nightly. Releases are newest-first,
 * as GitHub lists them. A release without an asset for this machine is
 * skipped, so a half-published release never breaks a fresh start.
 */
export function pickRelease(
  releases: readonly ReleaseSummary[],
  channel: Channel,
  target: string,
): string | null {
  const asset = `${target}.tar.gz`
  for (const release of releases) {
    if (release.draft) continue
    const nightly = release.tag_name.includes('-nightly.')
    if (channel === 'stable' && release.prerelease) continue
    if (channel === 'beta' && nightly) continue
    if (channel === 'nightly' && !nightly) continue
    if (release.assets && !release.assets.some((a) => a.name === asset)) continue
    return release.tag_name
  }
  return null
}

/** The digest from a `<sha256>  <file>` checksum file. */
export function parseChecksum(content: string): string {
  const digest = content.trim().split(/\s+/)[0]?.toLowerCase() ?? ''
  if (!/^[0-9a-f]{64}$/.test(digest)) {
    throw new StartError(
      'download',
      'bad_checksum_file',
      `The release checksum file does not contain a SHA-256 digest (got "${content.trim().slice(0, 80)}").`,
    )
  }
  return digest
}

/**
 * The first JSON object in a command's stdout. The server binary may print a
 * log line (for example which Docker context it uses) before its JSON.
 */
export function parseJsonOutput<T>(stdout: string, command: string): T {
  const start = stdout.indexOf('{')
  const end = stdout.lastIndexOf('}')
  if (start < 0 || end < start) {
    throw new StartError('setup', 'no_json_output', `\`temps ${command}\` printed no JSON result.`)
  }
  try {
    return JSON.parse(stdout.slice(start, end + 1)) as T
  } catch (error) {
    throw new StartError(
      'setup',
      'bad_json_output',
      `\`temps ${command}\` printed JSON that could not be parsed: ${(error as Error).message}`,
    )
  }
}

const ALPHANUMERIC = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789'

function randomString(length: number, bytes: (n: number) => Buffer = randomBytes): string {
  const source = bytes(length)
  let out = ''
  for (let i = 0; i < length; i++) out += ALPHANUMERIC[(source[i] ?? 0) % ALPHANUMERIC.length]
  return out
}

/**
 * An admin password that always meets the server's complexity rules: it
 * contains an upper-case letter, a lower-case letter, a digit and a symbol.
 */
export function generateAdminPassword(bytes?: (n: number) => Buffer): string {
  return `Tl-${randomString(20, bytes)}-9a`
}

export function generateDbPassword(bytes: (n: number) => Buffer = randomBytes): string {
  return bytes(24).toString('hex')
}

/** A `--port` value: digits only, 1–65535. `8080abc` and `8080.5` are typos, not port 8080. */
export function parsePort(value: string): number {
  const port = /^\d{1,5}$/.test(value) ? Number(value) : Number.NaN
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new StartError('preflight', 'bad_port', `--port must be a whole number from 1 to 65535 (got "${value}").`)
  }
  return port
}

/** Whether parsed JSON has the shape of a state file this version wrote. */
export function isLocalState(value: unknown): value is LocalState {
  if (typeof value !== 'object' || value === null) return false
  const state = value as Record<string, unknown>
  const port = (v: unknown) => Number.isInteger(v) && (v as number) > 0 && (v as number) < 65536
  const text = (v: unknown) => typeof v === 'string' && v.length > 0
  return (
    text(state.version) &&
    text(state.dbPassword) &&
    text(state.adminEmail) &&
    text(state.adminPassword) &&
    port(state.dbPort) &&
    port(state.proxyPort) &&
    port(state.consolePort) &&
    typeof state.setupComplete === 'boolean' &&
    (state.pid === undefined || (Number.isInteger(state.pid) && (state.pid as number) > 0))
  )
}

/** Where apps and the console are reached in the browser. */
export function localUrls(proxyPort: number): { external: string; console: string; app: (slug: string) => string } {
  const suffix = proxyPort === 80 ? '' : `:${proxyPort}`
  return {
    external: `http://${LOCAL_DOMAIN}${suffix}`,
    console: `http://console.${LOCAL_DOMAIN}${suffix}`,
    app: (slug: string) => `http://${slug}.${LOCAL_DOMAIN}${suffix}`,
  }
}

/** The API base URL the CLI context points at: the console listener, directly. */
export function consoleApiUrl(consolePort: number): string {
  return `http://127.0.0.1:${consolePort}/api`
}

export function databaseUrl(state: Pick<LocalState, 'dbPort' | 'dbPassword'>): string {
  return `postgresql://${DB_USER}:${encodeURIComponent(state.dbPassword)}@127.0.0.1:${state.dbPort}/${DB_NAME}?sslmode=disable`
}

/**
 * Ports to try for the app proxy, in order. Port 80 gives clean URLs, but
 * binding it needs root on Linux and is commonly taken on macOS, so it is
 * only tried for root on Linux; 8080 is what the installer's local mode
 * uses on macOS.
 */
export function proxyPortCandidates(preferred: number | undefined, platform: string, isRoot: boolean): number[] {
  const out: number[] = []
  if (preferred) out.push(preferred)
  if (platform === 'linux' && isRoot) out.push(80)
  for (let port = 8080; port < 8100; port++) out.push(port)
  return [...new Set(out)]
}

export function consolePortCandidates(): number[] {
  const out: number[] = []
  for (let port = 8700; port < 8720; port++) out.push(port)
  return out
}

export function dbPortCandidates(): number[] {
  const out: number[] = []
  for (let port = 55432; port < 55452; port++) out.push(port)
  return out
}

const PROJECT_MARKERS = [
  '.git',
  'package.json',
  'pyproject.toml',
  'requirements.txt',
  'go.mod',
  'Cargo.toml',
  'Gemfile',
  'composer.json',
  'Dockerfile',
  'index.html',
]

/**
 * Install the skills into the project when we're in one (so they travel with
 * the repo and only apply there), otherwise for the user. `none` skips them.
 */
export function resolveSkillsScope(
  requested: string | undefined,
  entries: readonly string[],
): SkillsScope {
  if (requested !== undefined) {
    if (requested === 'project' || requested === 'global' || requested === 'none') return requested
    throw new StartError(
      'preflight',
      'bad_skills_scope',
      `--skills must be project, global or none (got "${requested}").`,
    )
  }
  return entries.some((entry) => PROJECT_MARKERS.includes(entry)) ? 'project' : 'global'
}

/**
 * The one skill `temps start` installs. It covers the whole product; the
 * other folders under `skills/` are older, narrower skills it replaces, so
 * they are neither bundled nor installed.
 */
export const BUNDLED_SKILL = 'temps'

/** Agent skill directories: Claude Code reads `.claude/skills`; Codex and OpenCode read `.agents/skills`. */
export function skillTargets(base: string): string[] {
  return [join(base, '.claude', 'skills'), join(base, '.agents', 'skills')]
}

export function parseChannel(value: string | undefined): Channel {
  const channel = (value ?? 'beta') as Channel
  if (!CHANNELS.includes(channel)) {
    throw new StartError(
      'preflight',
      'bad_channel',
      `--channel must be one of ${CHANNELS.join(', ')} (got "${value}").`,
    )
  }
  return channel
}

/** Seconds with one decimal, as shown next to each finished step. */
export function formatSeconds(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`
}
