// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { writeFile, readFile } from 'node:fs/promises'
import { requireAuth, credentials } from '../../config/store.js'
import { normalizeApiUrl } from '../../lib/api-client.js'
import { config } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { isTTY } from '../../utils/tty.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
  keyValue,
} from '../../ui/output.js'
import {
  resolveWorkspaceSource,
  resolveProjectIdBySlug,
} from './workspace-source.js'
import { shellAction } from './shell.js'
// Placement shapes (ADR-048) come from the generated client so they cannot
// drift from the server's schema.
import type {
  EvictionUnconfirmedContainer,
  NodeEvictionResponse,
  NodeSandboxesResponse,
  PlacementNode,
  SandboxPlacementResponse,
  UpdateSandboxPlacementBody,
} from '../../api/types.gen.js'

// ── Types mirroring /v1/sandbox/* responses ─────────────────────────────────

/**
 * The `sandbox` object the server actually returns — the
 * `@vercel/sandbox`-compatible shape, not the flat one this file used to
 * assume.
 *
 * These types were written against an older contract and never corrected,
 * because the singular-vs-plural URL bug meant no request ever came back
 * 200 for anyone to notice. Two traps worth naming:
 *
 * - `cwd`, not `work_dir`.
 * - `createdAt` / `timeout` are **epoch milliseconds** and a **duration in
 *   milliseconds**. There is no `expires_at` on the wire; it is derived.
 */
interface SandboxInner {
  id: string
  name: string
  status: string
  image?: string | null
  cwd: string
  /** Epoch milliseconds. */
  createdAt: number
  /** Idle timeout in milliseconds. */
  timeout: number
  backend?: string | null
  disk_size_mb?: number | null
  preview_url_template?: string
  preview_password_hint?: string | null
  agent_run_id?: number | null
  /** 'ephemeral' | 'workspace' — absent on servers older than ADR-036. */
  lifecycle?: string
  project_id?: number | null
  source_repo_url?: string | null
  /** Worker node id (ADR-048); `null` = control plane. Absent on older servers. */
  node_id?: number | null
  /** Hosting node name; `control-plane` for control-plane sandboxes. */
  node_name?: string
}

/** Single-sandbox responses are wrapped: `{ sandbox, routes }`. */
interface SandboxResponse {
  sandbox: SandboxInner
  routes?: Array<{ url: string; subdomain: string; port: number }>
}

/**
 * Flattened view for display, mirroring
 * `web/src/components/sandboxes/helpers.ts` so the CLI and the console
 * derive `expires_at` identically instead of drifting apart again.
 */
interface SandboxView {
  id: string
  name: string
  status: string
  image: string | null
  work_dir: string
  created_at: string
  expires_at: string
  preview_password_hint?: string | null
  lifecycle?: string
  project_id?: number | null
  source_repo_url?: string | null
  node_name: string
}

export function toSandboxView(inner: SandboxInner): SandboxView {
  return {
    id: inner.id,
    name: inner.name,
    status: inner.status,
    image: inner.image ?? null,
    work_dir: inner.cwd,
    created_at: new Date(inner.createdAt).toISOString(),
    // The server sends a duration, not a deadline.
    expires_at: new Date(inner.createdAt + inner.timeout).toISOString(),
    preview_password_hint: inner.preview_password_hint ?? undefined,
    lifecycle: inner.lifecycle,
    project_id: inner.project_id ?? null,
    source_repo_url: inner.source_repo_url ?? null,
    node_name: inner.node_name ?? CONTROL_PLANE_NODE_NAME,
  }
}

/** Name the API uses for the control plane in placement responses. */
export const CONTROL_PLANE_NODE_NAME = 'control-plane'

/** Node id the placement API uses for the control plane. */
export const CONTROL_PLANE_NODE_ID = 0

interface SetPreviewPasswordResponse {
  preview_password_hint: string
}

interface ListSandboxesResponse {
  sandboxes: SandboxInner[]
  pagination: { count: number; next: number | null; prev: number | null }
}

interface ExecResponse {
  exit_code: number
  stdout: string
  stderr: string
}

interface ExecDetachedResponse {
  job_id: string
}

interface ReadFileResponse {
  path: string
  contents_b64: string
  size: number
}

interface StatResponse {
  path: string
  exists: boolean
  is_dir: boolean
  is_file: boolean
  size: number
}

interface DomainResponse {
  url: string
}

// ── Fetch helpers ───────────────────────────────────────────────────────────

interface SandboxApi {
  baseUrl: string
  apiKey: string
}

async function auth(): Promise<SandboxApi> {
  await requireAuth()
  const apiKey = await credentials.getApiKey()
  if (!apiKey) {
    throw new Error('Not authenticated. Run `temps login` first.')
  }
  const baseUrl = normalizeApiUrl(config.get('apiUrl'))
  return { baseUrl, apiKey }
}

/**
 * Build the absolute URL for a sandbox API path.
 *
 * The route is `/v1/sandboxes` — plural. The singular form predates a
 * rename and matches nothing on any current server, so every command in
 * this group was returning 404 against a real instance. The Rust CLI's
 * `sandbox_url` already carried both the fix and the warning; this one
 * did not. Covered by a test below so it can't drift back.
 */
export function sandboxUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/+$/, '')}/v1/sandboxes${path}`
}

async function apiRequest<T>(
  api: SandboxApi,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Authorization', `Bearer ${api.apiKey}`)
  if (init.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  const response = await fetch(sandboxUrl(api.baseUrl, path), { ...init, headers })

  if (!response.ok) {
    throw await readApiError(response)
  }

  // 204 No Content — caller asked for T but there's no body.
  if (response.status === 204) {
    return undefined as T
  }

  return (await response.json()) as T
}

/** Problem `type` for a feature that worker-node sandboxes don't support yet. */
export const UNSUPPORTED_ON_WORKER_TYPE =
  'https://temps.sh/probs/sandbox-unsupported-on-worker-node'

/**
 * A non-2xx sandbox API response. The message is what the user reads
 * (`title — detail`); `status` and the parsed Problem body stay available
 * for commands that react to specific failures (eviction's 409 and 503).
 */
/** Problem type of a partial eviction (some sandboxes were left). */
export const EVICTION_INCOMPLETE_TYPE =
  'https://temps.sh/probs/sandbox-node-eviction-incomplete'

export class SandboxApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly problem: Record<string, unknown> | null,
  ) {
    super(message)
    this.name = 'SandboxApiError'
  }

  get type(): string | undefined {
    const t = this.problem?.type
    return typeof t === 'string' ? t : undefined
  }

  get detail(): string | undefined {
    const d = this.problem?.detail
    return typeof d === 'string' && d.length > 0 ? d : undefined
  }
}

export async function readApiError(response: Response): Promise<SandboxApiError> {
  const text = await response.text().catch(() => '')
  let problem: Record<string, unknown> | null = null
  try {
    const parsed: unknown = JSON.parse(text)
    if (parsed && typeof parsed === 'object') problem = parsed as Record<string, unknown>
  } catch {
    // Not JSON: fall through to the raw text.
  }
  if (!problem) {
    return new SandboxApiError(
      `HTTP ${response.status}: ${text || response.statusText}`,
      response.status,
      null,
    )
  }
  const title = typeof problem.title === 'string' ? problem.title : `HTTP ${response.status}`
  const detail =
    typeof problem.detail === 'string' && problem.detail.length > 0 ? ` — ${problem.detail}` : ''
  let message = `${title}${detail}`
  // The server's sentence says what is missing; add the one way around it.
  if (problem.type === UNSUPPORTED_ON_WORKER_TYPE) {
    message +=
      '\nThis sandbox runs on a worker node. Create one with `--node control-plane` to use this feature.'
  }
  return new SandboxApiError(message, response.status, problem)
}

/**
 * Parse repeated `--env KEY=VAL` options into an object. Values may
 * contain `=` (only the first `=` splits).
 */
function parseEnvPairs(pairs: string[] | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  if (!pairs) return out
  for (const p of pairs) {
    const idx = p.indexOf('=')
    if (idx <= 0) {
      throw new Error(`Invalid --env '${p}': expected KEY=VAL`)
    }
    const key = p.slice(0, idx)
    const value = p.slice(idx + 1)
    out[key] = value
  }
  return out
}

function statusColor(status: string): string {
  const s = status.toLowerCase()
  if (s === 'running') return colors.success(status)
  if (s === 'stopped' || s === 'destroyed') return colors.error(status)
  if (s === 'pending' || s === 'creating') return colors.warning(status)
  return status
}

/**
 * Generate a URL-safe preview password on the client. Uses
 * `crypto.getRandomValues` over a 64-symbol alphabet, so each character
 * carries 6 bits of entropy — 24 chars gives ~144 bits, comfortably past
 * brute-force range. Kept in sync with `web/src/components/sandboxes/
 * SandboxPreviewPasswordCard.tsx` so UI + CLI produce the same shape.
 *
 * Clamped to the server's [8, 256] range to surface typos early instead
 * of as a 400 round-trip.
 */
function generatePassword(length = 24): string {
  if (length < 8 || length > 256) {
    throw new Error('Password length must be between 8 and 256 characters')
  }
  const alphabet =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_'
  const buf = new Uint8Array(length)
  crypto.getRandomValues(buf)
  let out = ''
  for (let i = 0; i < length; i++) {
    out += alphabet[buf[i]! % alphabet.length]
  }
  return out
}

function base64Encode(bytes: Uint8Array): string {
  // Bun/Node both support Buffer.
  return Buffer.from(bytes).toString('base64')
}

function base64Decode(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, 'base64'))
}

// ── Commander wiring ────────────────────────────────────────────────────────

export function registerSandboxCommands(program: Command): void {
  const sandbox = program
    .command('sandbox')
    .description('Manage standalone sandboxes (/v1/sandbox API)')

  sandbox
    .command('create')
    .description('Create a new sandbox')
    .option('--image <image>', 'Docker image override (uses platform default when omitted)')
    .option('--name <name>', 'Display name for the sandbox')
    .option('--timeout <seconds>', 'Idle timeout in seconds (clamped to [60, 86400])')
    .option(
      '-e, --env <KEY=VAL>',
      'Env var baked into the container (repeatable)',
      (val: string, prev: string[]) => (prev ? [...prev, val] : [val]),
    )
    .option('--cpu-limit <cpu>', 'CPU limit (e.g., 0.5 for half a core)')
    .option('--memory-mb <mb>', 'Memory limit in megabytes')
    .option('--git-url <url>', 'Git repo URL to clone into the work dir')
    .option('--git-rev <revision>', 'Git revision to check out (requires --git-url)')
    .option('--git-depth <n>', 'Shallow clone depth (requires --git-url)')
    .option(
      '--git-connection <id>',
      'ID of a stored git provider connection; temps injects the token server-side',
    )
    .option('--git-username <user>', 'HTTP Basic username for private repo clone (requires --git-password)')
    .option('--git-password <token>', 'HTTP Basic password/token (paired with --git-username; injected via GIT_ASKPASS)')
    .option('--tarball-url <url>', 'Tarball URL to download and extract')
    .option(
      '--workspace',
      'Create a persistent workspace: suspends when idle, wakes automatically on the next command, and is never destroyed for you',
    )
    .option(
      '--project <slug>',
      "Seed from a temps project's connected repo (and attribute the sandbox to it). Defaults to the linked project in .temps/config.json",
    )
    .option(
      '--repo <owner/name>',
      'Seed from a repo on one of your git connections that has no temps project',
    )
    .option('--branch <ref>', 'Branch, tag, or SHA to check out (alias of --git-rev)')
    .option(
      '--new-branch <name>',
      'Create and switch to a new branch after cloning, based on whatever was checked out',
    )
    .option(
      '--preview-password',
      'Generate a random preview-URL password and print it once on stdout',
    )
    .option(
      '--preview-password-length <n>',
      'Length of the generated preview password (8..=256, default 24)',
    )
    .option(
      '--from-snapshot <snap-id>',
      'Create sandbox from a snapshot (mutually exclusive with --image)',
    )
    .option(
      '--node <name|id>',
      "Node to run the sandbox on (name, id, or 'control-plane'). Omit to let Temps place it; see `sandbox nodes`",
    )
    .option('--json', 'Output as JSON')
    .action(createAction)

  sandbox
    .command('list')
    .alias('ls')
    .description('List your sandboxes')
    .option('--page <n>', 'Page (1-indexed)')
    .option('--page-size <n>', 'Items per page (default 20, max 100)')
    .option('--workspace', 'Show only persistent workspaces')
    .option('--lifecycle <class>', 'Filter by lifecycle class: ephemeral | workspace')
    .option('--project <slug>', 'Show only sandboxes created from this project')
    .option('--json', 'Output as JSON')
    .action(listAction)

  // `--json` lives on each subcommand, never on `nodes` itself: commander
  // lets a parent consume its own options anywhere on the line, so a parent
  // `--json` would silently swallow `nodes show x --json`.
  const nodes = sandbox
    .command('nodes')
    .description('List nodes that can run sandboxes and control which ones take new sandboxes')

  nodes
    .command('list', { isDefault: true })
    .description('List nodes with their sandbox placement state (the default)')
    .option('--json', 'Output as JSON')
    .action(nodesListAction)

  nodes
    .command('show <node>')
    .description(
      "Show a node's placement state and every live sandbox on it, from all users (name, id, or 'control-plane'). Admin only",
    )
    .option('--page <n>', 'Page number (default 1)')
    .option('--page-size <n>', 'Items per page (default 20, max 100)')
    .option('--json', 'Output as JSON')
    .action(nodesShowAction)

  nodes
    .command('set <nodes...>')
    .description(
      "Allow exactly these nodes to run new sandboxes, replacing the list (names, ids, or 'control-plane'). Admin only",
    )
    .option('--json', 'Output as JSON')
    .action((refs: string[], options: { json?: boolean }) => nodesChangeAction('set', refs, options))

  nodes
    .command('allow <nodes...>')
    .description('Add nodes to the list allowed to run new sandboxes. Admin only')
    .option('--json', 'Output as JSON')
    .action((refs: string[], options: { json?: boolean }) =>
      nodesChangeAction('allow', refs, options),
    )

  nodes
    .command('deny <nodes...>')
    .description('Stop nodes from taking new sandboxes (existing ones keep running). Admin only')
    .option('--json', 'Output as JSON')
    .action((refs: string[], options: { json?: boolean }) => nodesChangeAction('deny', refs, options))

  nodes
    .command('allow-all')
    .description('Allow every node, including the control plane, to run sandboxes (the default). Admin only')
    .option('--json', 'Output as JSON')
    .action((options: { json?: boolean }) => updatePlacement(null, options))

  nodes
    .command('deny-all')
    .description('Stop every node from taking new sandboxes (existing ones keep running). Admin only')
    .option('--json', 'Output as JSON')
    .action((options: { json?: boolean }) => updatePlacement([], options))

  nodes
    .command('evict <node>')
    .description(
      'Destroy every sandbox on a worker node, from all users, so the node can be removed. Works on a node that is offline for good. Admin only',
    )
    .option('-f, --force', 'Skip confirmation prompt')
    .option('--json', 'Output as JSON')
    .action(nodesEvictAction)

  sandbox
    .command('show <id>')
    .description('Show details for a sandbox')
    .option('--json', 'Output as JSON')
    .action(showAction)

  sandbox
    .command('rm <id>')
    .aliases(['stop', 'destroy'])
    .description('Remove a sandbox permanently (aliases: stop, destroy)')
    .option('-f, --force', 'Skip confirmation prompt')
    .action(rmAction)

  sandbox
    .command('pause <id>')
    .description('Pause a running sandbox (non-destructive — resume later with `sandbox resume`)')
    .action(pauseAction)

  sandbox
    .command('resume <id>')
    .description('Resume a paused sandbox')
    .action(resumeAction)

  sandbox
    .command('restart <id>')
    .description('Restart a running sandbox (preserves filesystem)')
    .action(restartAction)

  sandbox
    .command('clone <id>')
    .description('Clone a git repo or extract a tarball into a running sandbox')
    .option('--git-url <url>', 'Git repo URL to clone')
    .option('--git-rev <revision>', 'Git revision (branch/tag/SHA) to check out')
    .option('--git-depth <n>', 'Shallow clone depth')
    .option(
      '--git-connection <id>',
      'ID of a stored git provider connection; temps injects the token server-side',
    )
    .option('--git-username <user>', 'HTTP Basic username (pairs with --git-password)')
    .option('--git-password <token>', 'HTTP Basic password/token (injected via GIT_ASKPASS)')
    .option('--tarball-url <url>', 'Tarball URL to download and extract')
    .action(cloneAction)

  sandbox
    .command('shell <id>')
    .alias('attach')
    .description(
      'Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab',
    )
    .option(
      '--tab <name>',
      'Tab to attach to; reusing a name reattaches to the program already running in it',
      'main',
    )
    .option(
      '--cmd <command>',
      'Program to start when the tab is created, e.g. "claude" (default: login shell)',
    )
    .action(shellAction)

  sandbox
    .command('extend <id>')
    .description("Extend a sandbox's idle timeout")
    .requiredOption('--secs <seconds>', 'Extra seconds to add to the current expiry')
    .action(extendAction)

  sandbox
    .command('exec <id> [args...]')
    .description('Run a command inside a sandbox. Use `--` to pass flags: `exec ID -- ls -la`')
    .option('--detach', 'Start in background and print a job ID instead of waiting')
    .option('--cwd <path>', 'Working directory inside the sandbox')
    .option(
      '-e, --env <KEY=VAL>',
      'Env var for this exec (repeatable)',
      (val: string, prev: string[]) => (prev ? [...prev, val] : [val]),
    )
    .action(execAction)

  sandbox
    .command('logs <id> <jobId>')
    .description('Stream logs from a detached job (SSE)')
    .action(logsAction)

  sandbox
    .command('domain <id>')
    .description('Resolve the preview URL for a port inside a sandbox')
    .requiredOption('--port <port>', 'Port inside the sandbox (1..=65535)')
    .action(domainAction)

  sandbox
    .command('password <id>')
    .description(
      'Generate, rotate, or clear the preview-URL password for a sandbox',
    )
    .option(
      '--rotate',
      'Generate a new random password and set it (default when no flag is given)',
    )
    .option('--length <n>', 'Length of the generated password (8..=256, default 24)')
    .option('--clear', 'Remove the preview password — preview URLs become open again')
    .action(passwordAction)

  // ── Filesystem subgroup ──
  const fs = sandbox.command('fs').description('Filesystem operations inside a sandbox')

  fs.command('read <id>')
    .description('Read a file from the sandbox')
    .requiredOption('--path <path>', 'Absolute file path inside the sandbox')
    .option('--out <localPath>', 'Write to this local file (stdout when omitted)')
    .action(fsReadAction)

  fs.command('write <id>')
    .description('Write a file to the sandbox')
    .requiredOption('--path <path>', 'Absolute target path inside the sandbox')
    .option('--file <localPath>', 'Local source file to upload (mutually exclusive with --content)')
    .option('--content <string>', 'Inline string content to write')
    .option('--mode <octal>', 'Unix permission mask (default: 0644)')
    .action(fsWriteAction)

  fs.command('stat <id>')
    .description('Stat a path inside the sandbox')
    .requiredOption('--path <path>', 'Absolute path inside the sandbox')
    .option('--json', 'Output as JSON')
    .action(fsStatAction)

  fs.command('mkdir <id>')
    .description('Create a directory inside the sandbox (mkdir -p)')
    .requiredOption('--path <path>', 'Absolute path inside the sandbox')
    .action(fsMkdirAction)

  // ── Snapshot subgroup (ADR-037) ──
  const snaps = sandbox.command('snapshots').description('Manage sandbox snapshots (ADR-037)')

  sandbox
    .command('snapshot <id>')
    .description('Take a snapshot of a sandbox')
    .option('--label <label>', 'Human-readable label for the snapshot')
    .option('--wait', 'Wait until the snapshot reaches ready or failed status')
    .option('--json', 'Output as JSON')
    .action(snapshotCreateAction)

  snaps
    .command('list')
    .alias('ls')
    .description('List your snapshots')
    .option('--project <id>', 'Filter by project ID')
    .option('--status <status>', 'Filter by status: creating | ready | failed | deleted')
    .option('--page <n>', 'Page number (1-indexed)')
    .option('--page-size <n>', 'Items per page (default 20, max 100)')
    .option('--json', 'Output as JSON')
    .action(snapshotListAction)

  snaps
    .command('show <snapId>')
    .description('Show details for a snapshot')
    .option('--json', 'Output as JSON')
    .action(snapshotShowAction)

  snaps
    .command('delete <snapId>')
    .alias('rm')
    .description('Delete a snapshot permanently')
    .option('-f, --force', 'Skip confirmation prompt')
    .action(snapshotDeleteAction)

  snaps
    .command('storage')
    .description('Show snapshot storage usage and quota')
    .option('--json', 'Output as JSON')
    .action(snapshotStorageAction)
}

// ── Actions ─────────────────────────────────────────────────────────────────

interface CreateOptions {
  image?: string
  name?: string
  timeout?: string
  env?: string[]
  cpuLimit?: string
  memoryMb?: string
  gitUrl?: string
  gitRev?: string
  gitDepth?: string
  gitConnection?: string
  gitUsername?: string
  gitPassword?: string
  tarballUrl?: string
  previewPassword?: boolean
  previewPasswordLength?: string
  workspace?: boolean
  project?: string
  repo?: string
  branch?: string
  newBranch?: string
  fromSnapshot?: string
  node?: string
  json?: boolean
}

/**
 * Shared between `create --git-*` and `clone --git-*`. Returns the
 * `source` body field or `null` when no source was requested, and
 * throws on validation errors the server would also reject (mutual
 * exclusion, missing --git-url, etc.).
 */
function buildSource(options: {
  gitUrl?: string
  gitRev?: string
  gitDepth?: string
  gitConnection?: string
  gitUsername?: string
  gitPassword?: string
  tarballUrl?: string
}): Record<string, unknown> | null {
  const gitFlags = [
    options.gitRev,
    options.gitDepth,
    options.gitConnection,
    options.gitUsername,
    options.gitPassword,
  ].filter((v) => v !== undefined)
  if (gitFlags.length > 0 && !options.gitUrl) {
    throw new Error('--git-* flags require --git-url')
  }
  if (options.gitUrl && options.tarballUrl) {
    throw new Error('--git-url and --tarball-url are mutually exclusive')
  }
  if (
    (options.gitUsername || options.gitPassword) &&
    options.gitConnection !== undefined
  ) {
    throw new Error('--git-connection is mutually exclusive with --git-username/--git-password')
  }
  if (
    (options.gitUsername && !options.gitPassword) ||
    (!options.gitUsername && options.gitPassword)
  ) {
    throw new Error('--git-username and --git-password must be provided together')
  }

  if (options.gitUrl) {
    const src: Record<string, unknown> = { type: 'git', url: options.gitUrl }
    if (options.gitRev) src.revision = options.gitRev
    if (options.gitDepth !== undefined) {
      const d = Number(options.gitDepth)
      if (!Number.isInteger(d) || d <= 0) {
        throw new Error('--git-depth must be a positive integer')
      }
      src.depth = d
    }
    if (options.gitConnection !== undefined) {
      const id = Number(options.gitConnection)
      if (!Number.isInteger(id) || id <= 0) {
        throw new Error('--git-connection must be a positive integer')
      }
      src.git_connection_id = id
    }
    if (options.gitUsername) src.username = options.gitUsername
    if (options.gitPassword) src.password = options.gitPassword
    return src
  }

  if (options.tarballUrl) {
    return { type: 'tarball', url: options.tarballUrl }
  }

  return null
}

async function createAction(options: CreateOptions): Promise<void> {
  const api = await auth()
  const env = parseEnvPairs(options.env)

  if (options.fromSnapshot && options.image) {
    throw new Error("--from-snapshot and --image are mutually exclusive")
  }

  const body: Record<string, unknown> = {}
  if (options.fromSnapshot) body.from_snapshot = options.fromSnapshot
  if (options.node) body.node = options.node
  if (options.image) body.image = options.image
  if (options.name) body.name = options.name
  if (options.timeout !== undefined) body.timeout_secs = Number(options.timeout)
  if (Object.keys(env).length > 0) body.env = env
  if (options.cpuLimit !== undefined) body.cpu_limit = Number(options.cpuLimit)
  if (options.memoryMb !== undefined) body.memory_limit_mb = Number(options.memoryMb)
  if (options.workspace) body.lifecycle = 'workspace'

  // A workspace always needs code, so resolve one even when no source flag
  // was passed — that's the whole point of `sandbox create --workspace` in
  // a linked checkout. A plain sandbox stays empty unless asked, so it
  // never turns into an interactive command.
  const wantsSource =
    options.workspace ||
    options.repo !== undefined ||
    options.project !== undefined ||
    options.gitUrl !== undefined ||
    options.tarballUrl !== undefined

  let origin: string | undefined
  if (wantsSource) {
    const resolved = await resolveWorkspaceSource(options)
    if (resolved.projectId !== undefined) body.project_id = resolved.projectId
    if (resolved.source) body.source = resolved.source
    origin = resolved.origin
  } else {
    // Preserves the "--git-* without --git-url" guard for stray flags.
    const source = buildSource(options)
    if (source) body.source = source
  }

  // Preview-password generation happens client-side so the plaintext
  // exists only on this machine: the server stores just an argon2 hash
  // and the 4-char hint. Printed once below — never retrievable later.
  let generatedPassword: string | undefined
  if (options.previewPassword) {
    const len =
      options.previewPasswordLength !== undefined
        ? Number(options.previewPasswordLength)
        : 24
    if (!Number.isInteger(len)) {
      throw new Error('--preview-password-length must be an integer')
    }
    generatedPassword = generatePassword(len)
    body.preview_password = generatedPassword
  }

  const envelope = await withSpinner(
    options.workspace ? 'Creating workspace...' : 'Creating sandbox...',
    () =>
      apiRequest<SandboxResponse>(api, '', {
        method: 'POST',
        body: JSON.stringify(body),
      }),
  )
  const sbx = toSandboxView(envelope.sandbox)

  // Branch creation is a post-clone step: the server seeds the work dir at
  // whatever ref was requested, then we branch off it. Non-fatal — the
  // workspace exists and is usable either way, so report and carry on
  // rather than leaving the user with a sandbox they think failed.
  let newBranchError: string | undefined
  if (options.newBranch) {
    try {
      const res = await apiRequest<ExecResponse>(
        api,
        `/${encodeURIComponent(sbx.id)}/exec`,
        {
          method: 'POST',
          body: JSON.stringify({
            cmd: ['git', 'checkout', '-b', options.newBranch],
            cwd: sbx.work_dir,
          }),
        },
      )
      if (res.exit_code !== 0) {
        newBranchError = res.stderr.trim() || `git exited ${res.exit_code}`
      }
    } catch (e) {
      newBranchError = e instanceof Error ? e.message : String(e)
    }
  }

  if (options.json) {
    // In JSON mode the generated plaintext is part of the payload so
    // scripts can capture it in one call. Caller is responsible for
    // handling it safely.
    json(
      generatedPassword
        ? { ...envelope, preview_password: generatedPassword }
        : envelope,
    )
    return
  }

  const createdWorkspace = sbx.lifecycle === 'workspace'
  success(
    `${createdWorkspace ? 'Workspace' : 'Sandbox'} ${colors.primary(sbx.id)} created`,
  )
  keyValue('Name', sbx.name)
  keyValue('Status', statusColor(sbx.status))
  keyValue('Image', sbx.image ?? '(default)')
  keyValue('Node', sbx.node_name)
  keyValue('Work dir', sbx.work_dir)
  keyValue(createdWorkspace ? 'Suspends at' : 'Expires', sbx.expires_at)
  // `origin` says where the code came from *and how we worked that out*
  // (flag, linked project, this directory's remote) — the inference is
  // only trustworthy if it's visible.
  if (origin) {
    keyValue('Source', origin)
  } else if (sbx.source_repo_url) {
    keyValue('Repo', sbx.source_repo_url)
  }
  if (options.newBranch && !newBranchError) {
    keyValue('Branch', `${options.newBranch} (created)`)
  }
  if (newBranchError) {
    newline()
    warning(
      `Workspace created, but creating branch '${options.newBranch}' failed: ${newBranchError}`,
    )
    info(
      `Retry inside it: temps sandbox exec ${sbx.id} -- git checkout -b ${options.newBranch}`,
    )
  }
  if (createdWorkspace) {
    newline()
    info(
      `Run a command in it with: temps sandbox exec ${sbx.id} -- <command>\n` +
        '  It suspends when idle and wakes on the next command — your files stay put.',
    )
  }
  if (generatedPassword) {
    newline()
    warning('Preview password (shown once — copy it now):')
    console.log(`  ${colors.primary(generatedPassword)}`)
    if (sbx.preview_password_hint) {
      keyValue('Hint', `ends in …${sbx.preview_password_hint}`)
    }
  } else if (sbx.preview_password_hint) {
    keyValue('Preview password', `ends in …${sbx.preview_password_hint}`)
  }
  newline()
}

interface PasswordOptions {
  rotate?: boolean
  length?: string
  clear?: boolean
}

/**
 * Rotate or clear a sandbox's preview-URL password. The CLI generates
 * the new plaintext locally and sends it to
 * `PUT /v1/sandbox/{id}/preview-password`; the server only ever sees —
 * and persists — the argon2 hash plus the 4-char hint.
 *
 * The new password is printed exactly once. There is no retrieval path:
 * rotating again replaces it, and losing it before it's copied means
 * rotating a second time.
 */
async function passwordAction(
  id: string,
  options: PasswordOptions,
): Promise<void> {
  if (options.clear && (options.rotate || options.length)) {
    throw new Error('--clear is mutually exclusive with --rotate/--length')
  }

  const api = await auth()

  if (options.clear) {
    await withSpinner('Clearing preview password...', () =>
      apiRequest<void>(
        api,
        `/${encodeURIComponent(id)}/preview-password`,
        { method: 'DELETE' },
      ),
    )
    success(`Preview password cleared for ${colors.primary(id)}`)
    info('Preview URLs are now open — the sandbox ID is the only gate.')
    return
  }

  // Default behavior when no flag is given: rotate. Matches what the
  // command description says and avoids a silent no-op.
  const len = options.length !== undefined ? Number(options.length) : 24
  if (!Number.isInteger(len)) {
    throw new Error('--length must be an integer')
  }
  const password = generatePassword(len)

  const res = await withSpinner('Setting preview password...', () =>
    apiRequest<SetPreviewPasswordResponse>(
      api,
      `/${encodeURIComponent(id)}/preview-password`,
      {
        method: 'PUT',
        body: JSON.stringify({ password }),
      },
    ),
  )

  success(`Preview password set for ${colors.primary(id)}`)
  newline()
  warning('Preview password (shown once — copy it now):')
  console.log(`  ${colors.primary(password)}`)
  keyValue('Hint', `ends in …${res.preview_password_hint}`)
  newline()
}

interface ListOptions {
  page?: string
  pageSize?: string
  workspace?: boolean
  lifecycle?: string
  project?: string
  json?: boolean
}

async function listAction(options: ListOptions): Promise<void> {
  const api = await auth()

  const qs: string[] = []
  if (options.page) qs.push(`page=${encodeURIComponent(options.page)}`)
  if (options.pageSize) qs.push(`page_size=${encodeURIComponent(options.pageSize)}`)
  // `--workspace` is shorthand for `--lifecycle workspace`. If both are
  // given, the explicit one wins rather than silently conflicting.
  const lifecycle = options.lifecycle ?? (options.workspace ? 'workspace' : undefined)
  if (lifecycle) qs.push(`lifecycle=${encodeURIComponent(lifecycle)}`)
  // Slug in, numeric id out — the filter is on `sandboxes.project_id`, but
  // every other `--project` in this CLI takes a slug and users shouldn't
  // have to know which is which.
  if (options.project) {
    const projectId = await resolveProjectIdBySlug(options.project)
    qs.push(`project_id=${projectId}`)
  }
  const path = qs.length ? `?${qs.join('&')}` : ''

  const data = await withSpinner('Fetching sandboxes...', () =>
    apiRequest<ListSandboxesResponse>(api, path),
  )

  if (options.json) {
    json(data)
    return
  }

  const items = (data.sandboxes ?? []).map(toSandboxView)

  newline()
  header(`${icons.info} Sandboxes (${data.pagination?.count ?? items.length})`)

  if (items.length === 0) {
    info('No sandboxes found. Create one with `temps sandbox create`.')
    newline()
    return
  }

  const columns: TableColumn<SandboxView>[] = [
    { header: 'ID', key: 'id', color: (v) => colors.primary(v) },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Status', key: 'status', color: (v) => statusColor(v) },
    {
      header: 'Kind',
      // Older servers don't send `lifecycle`; everything there is ephemeral.
      accessor: (s) => (s.lifecycle === 'workspace' ? 'workspace' : 'ephemeral'),
      color: (v) => (v === 'workspace' ? colors.primary(v) : colors.muted(v)),
    },
    { header: 'Node', key: 'node_name', color: (v) => colors.muted(v) },
    {
      header: 'Image',
      accessor: (s) => s.image ?? '(default)',
      color: (v) => colors.muted(v.length > 30 ? v.slice(0, 30) + '...' : v),
    },
    // For a workspace this is when it suspends, not when it dies — the
    // header would be misleading without that distinction, and the
    // `sandbox show` output spells it out.
    { header: 'Expires', key: 'expires_at', color: (v) => colors.muted(v) },
  ]

  printTable(items, columns, { style: 'minimal' })
  newline()
}

// ── Placement (ADR-048) ──────────────────────────────────────────────────────

/** `null` = every node; an absent member (older servers) means the same. */
function allowedIds(data: SandboxPlacementResponse): number[] | null {
  return data.allowed_node_ids ?? null
}

function printPlacement(data: SandboxPlacementResponse): void {
  const allowed = allowedIds(data)
  newline()
  header(`${icons.info} Sandbox nodes`)
  info(
    allowed === null
      ? 'Every node may run sandboxes (default).'
      : allowed.length === 0
        ? 'No node may run new sandboxes. Allow one with `sandbox nodes allow <node>` or `sandbox nodes allow-all`.'
        : `Only the nodes marked "yes" may run new sandboxes.`,
  )
  newline()
  const columns: TableColumn<PlacementNode>[] = [
    { header: 'ID', accessor: (n) => String(n.id), color: (v) => colors.muted(v) },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Status', key: 'status', color: (v) => (v === 'active' ? colors.success(v) : colors.warning(v)) },
    { header: 'Allowed', accessor: (n) => (n.allowed ? 'yes' : 'no') },
    { header: 'Sandboxes', accessor: (n) => String(n.live_sandboxes) },
    {
      header: 'Note',
      accessor: (n) => n.reason ?? (n.eligible ? 'accepts new sandboxes' : ''),
      color: (v) => colors.muted(v),
    },
  ]
  printTable(data.nodes, columns, { style: 'minimal' })
  if (data.nodes.length === 1) {
    newline()
    info(
      'Only the control plane is available. Add a worker with `temps join` on another machine ' +
        'to run sandboxes there (e.g. `sandbox create --node <worker>`).',
    )
  }
  newline()
}

async function nodesListAction(options: { json?: boolean }): Promise<void> {
  const api = await auth()
  const data = await withSpinner('Fetching sandbox nodes...', () =>
    apiRequest<SandboxPlacementResponse>(api, '/placement'),
  )
  if (options.json) {
    json(data)
    return
  }
  printPlacement(data)
}

/**
 * Resolve node references (name, id, `control-plane`) to ids using the
 * placement listing, so a typo fails here with the list of valid names
 * instead of as an opaque 400.
 */
export function resolveNodeIds(refs: string[], nodes: PlacementNode[]): number[] {
  const ids: number[] = []
  for (const raw of refs) {
    const ref = raw.trim()
    // Same rules as the server's `RequestedNode::parse`: control-plane
    // aliases first, then a bare number is an id, anything else a name.
    const match = CONTROL_PLANE_REFS.includes(ref)
      ? nodes.find((n) => n.is_control_plane)
      : /^\d+$/.test(ref)
        ? nodes.find((n) => !n.is_control_plane && n.id === Number(ref))
        : nodes.find((n) => !n.is_control_plane && n.name === ref)
    if (!match) {
      throw new Error(
        `Unknown node '${ref}'. Known nodes: ${nodes.map((n) => `${n.name} (${n.id})`).join(', ')}`,
      )
    }
    if (!ids.includes(match.id)) ids.push(match.id)
  }
  return ids
}

const CONTROL_PLANE_REFS = ['0', CONTROL_PLANE_NODE_NAME, 'control_plane', 'local']

/**
 * The warning for a change that takes the control plane out of the
 * allow-list, or `null` when it doesn't. Printed to stderr (also under
 * `--json`, so scripts still see it without corrupting stdout).
 */
export function controlPlaneExclusionWarning(
  current: number[] | null,
  next: number[] | null,
): string | null {
  const wasAllowed = current === null || current.includes(CONTROL_PLANE_NODE_ID)
  const staysAllowed = next === null || next.includes(CONTROL_PLANE_NODE_ID)
  if (!wasAllowed || staysAllowed) return null
  if (next.length === 0) {
    return (
      'No node, including the control plane, will take new sandboxes: creating a sandbox fails ' +
      'until you allow one (`sandbox nodes allow <node>` or `sandbox nodes allow-all`). ' +
      'Managed AI application workspaces still run on the control plane.'
    )
  }
  return (
    'The control plane will no longer take new sandboxes: sandboxes created without --node go to ' +
    'the allowed worker with the fewest sandboxes. On worker nodes the terminal, the agent runtime ' +
    '(Fleet), snapshots, preview URLs, disk resize and volumes are not available yet. ' +
    'Managed AI application workspaces still always run on the control plane.'
  )
}

/**
 * Replace the allow-list. `allowed_node_ids` is always sent, `null`
 * included: the server requires the member so an empty body can never reset
 * the list by accident.
 */
async function applyPlacement(
  api: SandboxApi,
  current: number[] | null,
  allowed: number[] | null,
  options: { json?: boolean },
): Promise<void> {
  const warn = controlPlaneExclusionWarning(current, allowed)
  if (warn) warning(warn)
  const body: UpdateSandboxPlacementBody = { allowed_node_ids: allowed }
  const data = await withSpinner('Updating sandbox placement...', () =>
    apiRequest<SandboxPlacementResponse>(api, '/placement', {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  )
  if (options.json) {
    json(data)
    return
  }
  success('Sandbox placement updated. Existing sandboxes keep running where they are.')
  printPlacement(data)
}

/** `allow-all` / `deny-all`: a fixed list, compared against the saved one. */
async function updatePlacement(
  allowed: number[] | null,
  options: { json?: boolean },
): Promise<void> {
  const api = await auth()
  const current = await apiRequest<SandboxPlacementResponse>(api, '/placement')
  await applyPlacement(api, allowedIds(current), allowed, options)
}

export type AllowListChange = 'set' | 'allow' | 'deny'

/**
 * The allow-list after a `set` / `allow` / `deny`. `null` = every node.
 * Denying from "every node" starts from the full node list.
 */
export function nextAllowList(
  change: AllowListChange,
  current: number[] | null,
  ids: number[],
  allNodeIds: number[],
): number[] | null {
  switch (change) {
    case 'set':
      return [...ids]
    case 'allow':
      if (current === null) return null
      return [...current, ...ids.filter((id) => !current.includes(id))]
    case 'deny':
      return (current ?? allNodeIds).filter((id) => !ids.includes(id))
  }
}

async function nodesChangeAction(
  change: AllowListChange,
  refs: string[],
  options: { json?: boolean },
): Promise<void> {
  const api = await auth()
  const current = await apiRequest<SandboxPlacementResponse>(api, '/placement')
  const ids = resolveNodeIds(refs, current.nodes)
  const saved = allowedIds(current)
  const next = nextAllowList(
    change,
    saved,
    ids,
    current.nodes.map((n) => n.id),
  )
  await applyPlacement(api, saved, next, options)
}

/**
 * Eviction destroys other users' data, so it needs a human `yes` or an
 * explicit `--force`. A prompt can't be answered without a terminal, and it
 * would corrupt `--json` output, so those cases fail with the fix instead.
 */
export function assertEvictConfirmable(
  options: { force?: boolean; json?: boolean },
  interactive: boolean,
): void {
  if (options.force) return
  if (options.json || !interactive) {
    throw new Error(
      'Refusing to destroy sandboxes without confirmation. Re-run with --force (-f).',
    )
  }
}

/** `--page`/`--page-size` must be positive integers. */
export function parsePageOption(raw: string | undefined, flag: string): number | undefined {
  if (raw === undefined) return undefined
  const n = Number(raw)
  if (!Number.isInteger(n) || n < 1) {
    throw new Error(`Invalid ${flag} '${raw}': expected a positive integer`)
  }
  return n
}

async function nodesEvictAction(
  node: string,
  options: { force?: boolean; json?: boolean },
): Promise<void> {
  assertEvictConfirmable(options, isTTY())
  const api = await auth()
  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Destroy every sandbox on node ${node}, including other users' sandboxes?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  let data: NodeEvictionResponse
  try {
    data = await withSpinner(`Destroying sandboxes on ${node}...`, () =>
      apiRequest<NodeEvictionResponse>(api, `/placement/nodes/${encodeURIComponent(node)}/evict`, {
        method: 'POST',
      }),
    )
  } catch (e) {
    if (e instanceof SandboxApiError && e.status === 409) {
      throw new Error(
        `Sandboxes on node ${node} are already being destroyed by another eviction. ` +
          `Wait for it to finish, then check with \`sandbox nodes show ${node}\`.` +
          (e.detail ? `\n${e.detail}` : ''),
      )
    }
    // Only the partial-eviction problem carries a report; any other 503
    // (the sandbox subsystem is unavailable, say) destroyed nothing.
    if (e instanceof SandboxApiError && e.type === EVICTION_INCOMPLETE_TYPE) {
      const report = evictionReportFromProblem(node, e.problem)
      if (options.json) json(report)
      else printEvictionReport(report)
      throw new Error(
        `Eviction of node ${node} is incomplete` +
          (report.failed.length > 0
            ? `: ${report.failed.length} sandbox(es) could not be destroyed.`
            : '.') +
          ` Run \`sandbox nodes evict ${node} --force\` again to retry them.`,
      )
    }
    throw e
  }
  if (options.json) {
    json(data)
    return
  }
  printEvictionReport({
    node: data.node.name,
    destroyed: data.destroyed,
    containers_unconfirmed: data.containers_unconfirmed ?? [],
    failed: [],
    partial: false,
  })
}

/** A sandbox an eviction could not destroy; running it again retries it. */
export interface EvictionFailure {
  sandbox_id: string
  reason: string
}

/**
 * What an eviction did, from a full (200) or partial (503) one. Printed the
 * same way for both so the cleanup commands are never lost.
 */
export interface EvictionReport {
  node: string
  destroyed: string[]
  containers_unconfirmed: EvictionUnconfirmedContainer[]
  failed: EvictionFailure[]
  /** Some sandboxes could not be destroyed. */
  partial: boolean
  /** The server's sentence, for servers that predate the structured members. */
  detail?: string
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : []
}

function recordList(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((v): v is Record<string, unknown> => v !== null && typeof v === 'object')
    : []
}

/**
 * Read a partial eviction's Problem body. The extension members
 * (`destroyed`, `containers_unconfirmed`, `failed`) are optional: older
 * servers send only `detail`, which is then shown as is.
 */
export function evictionReportFromProblem(
  node: string,
  problem: Record<string, unknown> | null,
): EvictionReport {
  const p = problem ?? {}
  return {
    node,
    destroyed: stringList(p.destroyed),
    containers_unconfirmed: recordList(p.containers_unconfirmed).flatMap((c) =>
      typeof c.sandbox_id === 'string' && typeof c.cleanup_command === 'string'
        ? [
            {
              sandbox_id: c.sandbox_id,
              cleanup_command: c.cleanup_command,
              reason: typeof c.reason === 'string' ? c.reason : '',
            },
          ]
        : [],
    ),
    failed: recordList(p.failed).flatMap((f) =>
      typeof f.sandbox_id === 'string'
        ? [{ sandbox_id: f.sandbox_id, reason: typeof f.reason === 'string' ? f.reason : '' }]
        : [],
    ),
    partial: true,
    detail: typeof p.detail === 'string' && p.detail.length > 0 ? p.detail : undefined,
  }
}

function printEvictionReport(report: EvictionReport): void {
  const { destroyed, containers_unconfirmed: unconfirmed, failed } = report
  const structured = destroyed.length + unconfirmed.length + failed.length > 0
  if (report.partial) {
    warning(
      `Destroyed ${destroyed.length} sandbox(es) on ${colors.bold(report.node)}, but not all of them.`,
    )
    if (!structured && report.detail) info(report.detail)
  } else {
    success(`Destroyed ${destroyed.length} sandbox(es) on ${colors.bold(report.node)}.`)
  }
  if (report.partial && destroyed.length > 0) {
    info(`Destroyed: ${destroyed.join(', ')}`)
  }
  if (failed.length > 0) {
    warning(`Could not destroy ${failed.length} sandbox(es):`)
    for (const f of failed) info(`  ${f.sandbox_id}${f.reason ? `: ${f.reason}` : ''}`)
  }
  if (unconfirmed.length > 0) {
    warning(
      `The node did not confirm removing ${unconfirmed.length} container(s); they may still be running there:`,
    )
    for (const c of unconfirmed) info(`  ${c.sandbox_id}${c.reason ? `: ${c.reason}` : ''}`)
    info('Nothing in Temps tracks them any more. If the node comes back, run these on it to remove them:')
    for (const c of unconfirmed) info(`  ${c.cleanup_command}`)
    info('If the node is gone for good, remove it.')
  }
  if (!report.partial) {
    info('Sandboxes no longer block removing this node. Drain it first if it still runs deployments.')
  }
}

async function nodesShowAction(
  node: string,
  options: { json?: boolean; page?: string; pageSize?: string },
): Promise<void> {
  const page = parsePageOption(options.page, '--page')
  const pageSize = parsePageOption(options.pageSize, '--page-size')
  const api = await auth()
  const query = new URLSearchParams()
  if (page) query.set('page', String(page))
  if (pageSize) query.set('page_size', String(pageSize))
  const qs = query.toString() ? `?${query}` : ''
  const data = await withSpinner('Fetching node sandboxes...', () =>
    apiRequest<NodeSandboxesResponse>(api, `/placement/nodes/${encodeURIComponent(node)}${qs}`),
  )
  if (options.json) {
    json(data)
    return
  }
  const n = data.node
  newline()
  header(`${icons.info} ${n.is_control_plane ? 'Control plane' : n.name} (${n.id})`)
  keyValue('Status', n.status === 'active' ? colors.success(n.status) : colors.warning(n.status))
  keyValue(
    'Runs new sandboxes',
    n.eligible ? colors.success('yes') : colors.warning(n.reason ?? 'no'),
  )
  newline()
  header(`Sandboxes (${data.total})`)
  if (data.sandboxes.length === 0 && data.total > 0) {
    const pages = Math.ceil(data.total / data.page_size)
    info(`Page ${data.page} is past the last page (${pages}). Try --page ${pages}.`)
    newline()
    return
  }
  if (data.sandboxes.length === 0) {
    info(
      n.eligible
        ? `No sandboxes on this node. Create one with \`sandbox create --node ${n.is_control_plane ? 'control-plane' : n.name}\`.`
        : 'No sandboxes on this node.',
    )
    newline()
    return
  }
  type Row = SandboxView & { owner: string }
  const rows: Row[] = data.sandboxes.map((e) => ({
    ...toSandboxView(e.sandbox),
    owner: e.owner_email ?? (e.owner_user_id != null ? `user ${e.owner_user_id}` : 'system'),
  }))
  const columns: TableColumn<Row>[] = [
    { header: 'ID', key: 'id', color: (v) => colors.primary(v) },
    { header: 'Status', key: 'status', color: (v) => statusColor(v) },
    {
      header: 'Kind',
      accessor: (r) => (r.lifecycle === 'workspace' ? 'workspace' : 'ephemeral'),
      color: (v) => colors.muted(v),
    },
    { header: 'Owner', key: 'owner' },
    { header: 'Expires', key: 'expires_at', color: (v) => colors.muted(v) },
  ]
  printTable(rows, columns, { style: 'minimal' })
  if (data.total > data.sandboxes.length) {
    newline()
    const pages = Math.ceil(data.total / data.page_size)
    info(
      `Page ${data.page} of ${pages} (${data.total} sandboxes).` +
        (data.page < pages ? ` Next: --page ${data.page + 1}` : ''),
    )
  }
  newline()
}


async function showAction(id: string, options: { json?: boolean }): Promise<void> {
  const api = await auth()
  const envelope = await withSpinner('Fetching sandbox...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}`),
  )

  if (options.json) {
    // JSON mode passes the server's envelope through untouched — scripts
    // should see the real API shape, not our display projection.
    json(envelope)
    return
  }

  const sbx = toSandboxView(envelope.sandbox)

  newline()
  header(`${icons.info} ${sbx.id}`)
  keyValue('Name', sbx.name)
  keyValue('Status', statusColor(sbx.status))
  keyValue('Image', sbx.image ?? '(default)')
  keyValue('Node', sbx.node_name)
  keyValue('Work dir', sbx.work_dir)
  keyValue('Created', sbx.created_at)
  const isWorkspace = sbx.lifecycle === 'workspace'
  keyValue('Kind', isWorkspace ? 'workspace (persistent)' : 'ephemeral')
  // Same column, two meanings. Spelling it out here is the difference
  // between "my sandbox is about to be deleted" panic and understanding
  // that a workspace just goes to sleep.
  keyValue(isWorkspace ? 'Suspends at' : 'Expires', sbx.expires_at)
  if (sbx.project_id !== undefined && sbx.project_id !== null) {
    keyValue('Project', String(sbx.project_id))
  }
  if (sbx.source_repo_url) {
    keyValue('Repo', sbx.source_repo_url)
  }
  if (sbx.preview_password_hint) {
    keyValue('Preview password', `ends in …${sbx.preview_password_hint}`)
  }
  if (isWorkspace) {
    newline()
    info(
      'Persistent workspace: it suspends after the idle timeout and wakes automatically ' +
        'on the next exec or file operation. Your files persist until you run `sandbox rm`.',
    )
  }
  newline()
}

async function rmAction(id: string, options: { force?: boolean }): Promise<void> {
  const api = await auth()

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Remove sandbox ${id} permanently?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing sandbox...', () =>
    apiRequest<void>(api, `/${encodeURIComponent(id)}/stop`, { method: 'POST' }),
  )
  success(`Sandbox ${colors.primary(id)} removed`)
}

async function pauseAction(id: string): Promise<void> {
  const api = await auth()
  const envelope = await withSpinner('Pausing sandbox...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}/pause`, { method: 'POST' }),
  )
  const sbx = toSandboxView(envelope.sandbox)
  success(`Sandbox ${colors.primary(id)} paused — status: ${statusColor(sbx.status)}`)
  info(`Resume with: temps sandbox resume ${id}`)
}

async function resumeAction(id: string): Promise<void> {
  const api = await auth()
  const envelope = await withSpinner('Resuming sandbox...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}/resume`, { method: 'POST' }),
  )
  const sbx = toSandboxView(envelope.sandbox)
  success(`Sandbox ${colors.primary(id)} resumed — expires: ${sbx.expires_at}`)
}

async function restartAction(id: string): Promise<void> {
  const api = await auth()
  const envelope = await withSpinner('Restarting sandbox...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}/restart`, { method: 'POST' }),
  )
  const sbx = toSandboxView(envelope.sandbox)
  success(`Sandbox ${colors.primary(id)} restarted — status: ${statusColor(sbx.status)}`)
}

interface CloneOptions {
  gitUrl?: string
  gitRev?: string
  gitDepth?: string
  gitConnection?: string
  gitUsername?: string
  gitPassword?: string
  tarballUrl?: string
}

async function cloneAction(id: string, options: CloneOptions): Promise<void> {
  const source = buildSource(options)
  if (!source) {
    throw new Error('Provide --git-url or --tarball-url')
  }
  const api = await auth()
  const sbx = await withSpinner('Seeding source...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}/source`, {
      method: 'POST',
      body: JSON.stringify(source),
    }),
  )
  const view = toSandboxView(sbx.sandbox)
  success(`Source seeded into ${colors.primary(view.id)}`)
  keyValue('Work dir', view.work_dir)
}

async function extendAction(id: string, options: { secs: string }): Promise<void> {
  const api = await auth()
  const extra = Number(options.secs)
  if (!Number.isFinite(extra) || extra <= 0) {
    throw new Error('--secs must be a positive number')
  }

  const sbx = await withSpinner('Extending timeout...', () =>
    apiRequest<SandboxResponse>(api, `/${encodeURIComponent(id)}/extend-timeout`, {
      method: 'POST',
      body: JSON.stringify({ extra_secs: extra }),
    }),
  )
  success(`Extended by ${extra}s — new expiry: ${toSandboxView(sbx.sandbox).expires_at}`)
}

interface ExecOptions {
  detach?: boolean
  cwd?: string
  env?: string[]
}

async function execAction(
  id: string,
  args: string[],
  options: ExecOptions,
): Promise<void> {
  if (!args || args.length === 0) {
    throw new Error(
      'Provide a command to run. Example: `temps sandbox exec ID -- ls -la`',
    )
  }

  const api = await auth()
  const env = parseEnvPairs(options.env)

  const body: Record<string, unknown> = { cmd: args }
  if (Object.keys(env).length > 0) body.env = env
  if (options.cwd) body.cwd = options.cwd

  const path = options.detach
    ? `/${encodeURIComponent(id)}/exec-detached`
    : `/${encodeURIComponent(id)}/exec`

  if (options.detach) {
    const res = await withSpinner('Starting detached job...', () =>
      apiRequest<ExecDetachedResponse>(api, path, {
        method: 'POST',
        body: JSON.stringify(body),
      }),
    )
    success(`Detached job started: ${colors.primary(res.job_id)}`)
    info(`Stream logs: temps sandbox logs ${id} ${res.job_id}`)
    return
  }

  const res = await apiRequest<ExecResponse>(api, path, {
    method: 'POST',
    body: JSON.stringify(body),
  })

  if (res.stdout) process.stdout.write(res.stdout)
  if (res.stderr) process.stderr.write(res.stderr)

  if (res.exit_code !== 0) {
    process.exit(res.exit_code)
  }
}

/**
 * Stream logs via Server-Sent Events. The server emits `event: log`
 * frames with JSON data `{ stream: 'stdout'|'stderr', data: '<line>' }`,
 * `event: lagged` when the broadcast buffer overflows, and `event: done`
 * when the job finishes.
 */
async function logsAction(id: string, jobId: string): Promise<void> {
  const api = await auth()

  const response = await fetch(
    sandboxUrl(api.baseUrl, `/${encodeURIComponent(id)}/jobs/${encodeURIComponent(jobId)}/logs`),
    {
      headers: {
        Authorization: `Bearer ${api.apiKey}`,
        Accept: 'text/event-stream',
      },
    },
  )

  if (!response.ok) {
    throw await readApiError(response)
  }
  if (!response.body) {
    throw new Error('Server returned no response body for log stream')
  }

  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let eventName = ''

  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      let idx: number
      while ((idx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, idx).replace(/\r$/, '')
        buffer = buffer.slice(idx + 1)

        if (line === '') {
          // End of event frame
          eventName = ''
          continue
        }

        if (line.startsWith('event:')) {
          eventName = line.slice(6).trim()
          continue
        }

        if (!line.startsWith('data:')) continue
        const data = line.slice(5).replace(/^\s/, '')

        if (eventName === 'log') {
          try {
            const payload = JSON.parse(data) as { stream?: string; data?: string }
            const text = (payload.data ?? '') + '\n'
            if (payload.stream === 'stderr') {
              process.stderr.write(text)
            } else {
              process.stdout.write(text)
            }
          } catch {
            process.stdout.write(data + '\n')
          }
        } else if (eventName === 'lagged') {
          warning('Log subscriber fell behind; some lines were dropped')
        } else if (eventName === 'done') {
          return
        }
      }
    }
  } finally {
    reader.releaseLock()
  }
}

async function domainAction(id: string, options: { port: string }): Promise<void> {
  const api = await auth()
  const port = Number(options.port)
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error('--port must be an integer in 1..=65535')
  }

  const res = await apiRequest<DomainResponse>(
    api,
    `/${encodeURIComponent(id)}/domain?port=${port}`,
  )
  console.log(res.url)
}

async function fsReadAction(
  id: string,
  options: { path: string; out?: string },
): Promise<void> {
  const api = await auth()
  const res = await withSpinner('Reading file...', () =>
    apiRequest<ReadFileResponse>(
      api,
      `/${encodeURIComponent(id)}/fs/read?path=${encodeURIComponent(options.path)}`,
    ),
  )
  const bytes = base64Decode(res.contents_b64)

  if (options.out) {
    // The published CLI is bundled for Node (see docs.ts), so this must use
    // Node's fs promises rather than Bun.write, which is undefined there.
    await writeFile(options.out, bytes)
    success(`Wrote ${res.size} bytes to ${colors.primary(options.out)}`)
  } else {
    process.stdout.write(bytes)
  }
}

async function fsWriteAction(
  id: string,
  options: { path: string; file?: string; content?: string; mode?: string },
): Promise<void> {
  if (options.file && options.content !== undefined) {
    throw new Error('--file and --content are mutually exclusive')
  }

  let bytes: Uint8Array
  if (options.file) {
    // The published CLI is bundled for Node (see docs.ts), so this must use
    // Node's fs promises rather than Bun.file, which is undefined there.
    bytes = new Uint8Array(await readFile(options.file))
  } else if (options.content !== undefined) {
    bytes = new TextEncoder().encode(options.content)
  } else {
    throw new Error('Provide either --file <path> or --content <string>')
  }

  const body: Record<string, unknown> = {
    path: options.path,
    contents_b64: base64Encode(bytes),
  }
  if (options.mode !== undefined) {
    // Accept "0644", "644", or decimal. parseInt with base 8 if leading 0.
    const raw = options.mode
    const parsed = raw.startsWith('0') ? parseInt(raw, 8) : parseInt(raw, 10)
    if (!Number.isFinite(parsed)) {
      throw new Error(`--mode '${raw}' is not a valid permission mask`)
    }
    body.mode = parsed
  }

  const api = await auth()
  await withSpinner(`Writing ${bytes.length} bytes to ${options.path}...`, () =>
    apiRequest<void>(api, `/${encodeURIComponent(id)}/fs/write`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  )
  success(`Wrote ${bytes.length} bytes to ${colors.primary(options.path)}`)
}

async function fsStatAction(
  id: string,
  options: { path: string; json?: boolean },
): Promise<void> {
  const api = await auth()
  const res = await apiRequest<StatResponse>(
    api,
    `/${encodeURIComponent(id)}/fs/stat?path=${encodeURIComponent(options.path)}`,
  )

  if (options.json) {
    json(res)
    return
  }

  newline()
  keyValue('Path', res.path)
  keyValue('Exists', res.exists ? colors.success('yes') : colors.error('no'))
  if (res.exists) {
    const kind = res.is_dir ? 'directory' : res.is_file ? 'file' : 'other'
    keyValue('Type', kind)
    keyValue('Size', `${res.size} bytes`)
  }
  newline()
}

async function fsMkdirAction(id: string, options: { path: string }): Promise<void> {
  const api = await auth()
  await withSpinner(`Creating ${options.path}...`, () =>
    apiRequest<void>(api, `/${encodeURIComponent(id)}/fs/mkdir`, {
      method: 'POST',
      body: JSON.stringify({ path: options.path }),
    }),
  )
  success(`Directory ${colors.primary(options.path)} created`)
}

// ── Snapshot API helpers ─────────────────────────────────────────────────────

/**
 * Build URL for the flat `/v1/sandbox-snapshots/...` collection routes.
 * These live at a different root than `/v1/sandboxes/...`.
 */
function snapshotUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/+$/, '')}/v1/sandbox-snapshots${path}`
}

interface SnapshotResponse {
  id: string
  status: string
  backend: string
  label: string | null
  content_digest: string
  size_bytes: number
  image_ref: string | null
  // source_sandbox_id intentionally omitted: the backend omits the raw
  // internal DB integer to avoid leaking the source sandbox's sequential ID.
  project_id: number | null
  created_at: string
  updated_at: string
}

interface ListSnapshotsResponse {
  snapshots: SnapshotResponse[]
  total: number
  page: number
  page_size: number
}

interface StorageSummaryResponse {
  total_bytes: number
  snapshot_count: number
  quota_bytes: number
  // null when the platform disk-space check is not yet implemented (deferred).
  // Treat null as "unknown", not "zero bytes available".
  available_disk_bytes: number | null
}

async function snapshotApiRequest<T>(
  api: SandboxApi,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Authorization', `Bearer ${api.apiKey}`)
  if (init.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  const response = await fetch(snapshotUrl(api.baseUrl, path), { ...init, headers })

  if (!response.ok) {
    throw await readApiError(response)
  }

  if (response.status === 204) {
    return undefined as T
  }

  return (await response.json()) as T
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`
}

function snapshotStatusColor(status: string): string {
  if (status === 'ready') return colors.success(status)
  if (status === 'failed' || status === 'deleted') return colors.error(status)
  if (status === 'creating') return colors.warning(status)
  return status
}

// ── Snapshot actions ─────────────────────────────────────────────────────────

async function snapshotCreateAction(
  sandboxId: string,
  options: { label?: string; wait?: boolean; json?: boolean },
): Promise<void> {
  const api = await auth()
  const body: Record<string, unknown> = {}
  if (options.label) body.label = options.label

  const snap = await withSpinner('Creating snapshot…', () =>
    apiRequest<SnapshotResponse>(
      api,
      `/${encodeURIComponent(sandboxId)}/snapshots`,
      { method: 'POST', body: JSON.stringify(body) },
    ),
  )

  // Poll until ready/failed if --wait
  let finalSnap = snap
  if (options.wait && snap.status === 'creating') {
    finalSnap = await withSpinner('Waiting for snapshot to be ready…', async () => {
      let current = snap
      let attempts = 0
      const maxAttempts = 120 // 120 × 5s = 10 min
      while (current.status === 'creating' && attempts < maxAttempts) {
        await new Promise((r) => setTimeout(r, 5000))
        current = await snapshotApiRequest<SnapshotResponse>(api, `/${encodeURIComponent(current.id)}`)
        attempts++
      }
      return current
    })
  }

  if (options.json) {
    json(finalSnap)
    return
  }

  if (finalSnap.status === 'creating') {
    info(`Snapshot ${colors.primary(finalSnap.id)} is still being created.`)
    info(`Poll with: temps sandbox snapshots show ${finalSnap.id}`)
    info(`Create sandbox from it with: temps sandbox create --from-snapshot ${finalSnap.id}`)
  } else if (finalSnap.status === 'ready') {
    success(`Snapshot ${colors.primary(finalSnap.id)} is ready`)
  } else {
    warning(`Snapshot ${colors.primary(finalSnap.id)} is in status: ${finalSnap.status}`)
  }
  keyValue('ID', finalSnap.id)
  keyValue('Status', snapshotStatusColor(finalSnap.status))
  keyValue('Size', formatBytes(finalSnap.size_bytes))
  if (finalSnap.label) keyValue('Label', finalSnap.label)
  keyValue('Backend', finalSnap.backend)
  keyValue('Created', finalSnap.created_at)
}

async function snapshotListAction(options: {
  project?: string
  status?: string
  page?: string
  pageSize?: string
  json?: boolean
}): Promise<void> {
  const api = await auth()

  const params = new URLSearchParams()
  if (options.project) params.set('project_id', options.project)
  if (options.status) params.set('status', options.status)
  if (options.page) params.set('page', options.page)
  if (options.pageSize) params.set('page_size', options.pageSize)
  const qs = params.toString() ? `?${params.toString()}` : ''

  const res = await withSpinner('Fetching snapshots…', () =>
    snapshotApiRequest<ListSnapshotsResponse>(api, `${qs}`),
  )

  if (options.json) {
    json(res)
    return
  }

  if (res.snapshots.length === 0) {
    info('No snapshots found.')
    return
  }

  const columns: TableColumn<SnapshotResponse>[] = [
    { header: 'ID', accessor: (s) => colors.primary(s.id) },
    { header: 'Status', accessor: (s) => snapshotStatusColor(s.status) },
    { header: 'Backend', key: 'backend' },
    { header: 'Label', accessor: (s) => s.label ?? '-' },
    { header: 'Size', accessor: (s) => formatBytes(s.size_bytes) },
    { header: 'Created', key: 'created_at' },
  ]

  newline()
  printTable(res.snapshots, columns)
  newline()
  info(`Page ${res.page} of ${Math.ceil(res.total / res.page_size)} — ${res.total} total`)
}

async function snapshotShowAction(snapId: string, options: { json?: boolean }): Promise<void> {
  const api = await auth()
  const snap = await withSpinner('Fetching snapshot…', () =>
    snapshotApiRequest<SnapshotResponse>(api, `/${encodeURIComponent(snapId)}`),
  )

  if (options.json) {
    json(snap)
    return
  }

  newline()
  keyValue('ID', snap.id)
  keyValue('Status', snapshotStatusColor(snap.status))
  keyValue('Backend', snap.backend)
  if (snap.label) keyValue('Label', snap.label)
  keyValue('Size', formatBytes(snap.size_bytes))
  keyValue('Digest', snap.content_digest)
  if (snap.image_ref) keyValue('Image ref', snap.image_ref)
  keyValue('Created', snap.created_at)
  keyValue('Updated', snap.updated_at)
  newline()
  if (snap.status === 'ready') {
    info(`Restore with: temps sandbox create --from-snapshot ${snap.id}`)
  }
}

async function snapshotDeleteAction(snapId: string, options: { force?: boolean }): Promise<void> {
  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Delete snapshot ${snapId}? This cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Aborted.')
      return
    }
  }

  const api = await auth()
  await withSpinner('Deleting snapshot…', () =>
    snapshotApiRequest<void>(api, `/${encodeURIComponent(snapId)}`, { method: 'DELETE' }),
  )
  success(`Snapshot ${colors.primary(snapId)} deleted`)
}

async function snapshotStorageAction(options: { json?: boolean }): Promise<void> {
  const api = await auth()
  const summary = await withSpinner('Fetching storage summary…', () =>
    snapshotApiRequest<StorageSummaryResponse>(api, '/storage-summary'),
  )

  if (options.json) {
    json(summary)
    return
  }

  newline()
  keyValue('Snapshots', String(summary.snapshot_count))
  keyValue('Used', `${formatBytes(summary.total_bytes)} / ${formatBytes(summary.quota_bytes)}`)
  keyValue(
    'Available on disk',
    summary.available_disk_bytes !== null ? formatBytes(summary.available_disk_bytes) : 'unknown',
  )
  const pct = summary.quota_bytes > 0
    ? ((summary.total_bytes / summary.quota_bytes) * 100).toFixed(1)
    : '0.0'
  keyValue('Quota used', `${pct}%`)
  newline()
}
