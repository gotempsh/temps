// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Thin client + pure assertion helpers for the standalone sandbox API
 * (`/api/v1/sandboxes/*`, ADR-048 multi-node placement included).
 *
 * Why not the generated `@temps-sdk/api` SDK: that package is generated from
 * its own committed spec, which does not carry the ADR-048 surface yet
 * (`node` on create, `node_id`/`node_name` on responses, the
 * `/v1/sandboxes/placement*` routes), and the scenarios that use this module
 * assert on exact status codes and RFC 7807 `type` URIs, which the generated
 * functions hide behind `{ data, error }`. So this mirrors the CLI's own
 * `apiRequest` (apps/temps-cli/src/commands/sandbox/index.ts): typed local
 * interfaces over `fetch`, same `<base>/api/v1/sandboxes<path>` layout.
 *
 * Every request carries a hard client-side deadline (`AbortSignal.timeout`),
 * so a hung control plane or a frozen worker can never stall a scenario
 * indefinitely; a network error or deadline comes back as `status: 0` with the
 * reason in `text`, letting pollers treat it like any other non-success.
 */
import { normalizeApiUrl, type TempsClientConfig } from './client.ts'

/** Container-name prefix every sandbox (and its egress proxy) carries on its node. */
export const SANDBOX_CONTAINER_PREFIX = 'temps-sandbox-'
/** Public sandbox ids look like `sbx_<hex>`; the container label drops the prefix. */
export const SANDBOX_PUBLIC_ID_PREFIX = 'sbx_'
/** Working directory inside a sandbox container, bind-mounted from the node's work dir. */
export const SANDBOX_WORKSPACE_DIR = '/home/temps/workspace'

/** RFC 7807 `type` URIs the sandbox API returns (crates/temps-sandbox/src/handlers). */
export const SANDBOX_PROBLEM_TYPES = {
  nodeNotAllowed: 'https://temps.sh/probs/sandbox-node-not-allowed',
  nodeNotFound: 'https://temps.sh/probs/sandbox-node-not-found',
  nodeOffline: 'https://temps.sh/probs/sandbox-node-offline',
  nodeUnreachable: 'https://temps.sh/probs/sandbox-node-unreachable',
  noPlacementNode: 'https://temps.sh/probs/sandbox-no-placement-node',
  snapshotOnWorkerNode: 'https://temps.sh/probs/sandbox-snapshot-on-worker-node',
  unsupportedOnWorkerNode: 'https://temps.sh/probs/sandbox-unsupported-on-worker-node',
} as const

export interface ProblemBody {
  type?: string
  title?: string
  status?: number
  detail?: string
}

export interface SandboxInner {
  id: string
  name: string
  status: string
  /** `null` = control plane (ADR-048 §8). */
  node_id: number | null
  /** `"control-plane"` for control-plane sandboxes. */
  node_name: string
}

export interface SandboxResponse {
  sandbox: SandboxInner
}

export interface PlacementNode {
  id: number
  name: string
  is_control_plane: boolean
  status: string
  allowed: boolean
  eligible: boolean
  reason: string | null
  live_sandboxes: number
}

export interface SandboxPlacement {
  allowed_node_ids: number[] | null
  nodes: PlacementNode[]
}

export interface EvictionUnconfirmedContainer {
  sandbox_id: string
  reason: string
  cleanup_command: string
}

export interface NodeEvictionResponse {
  node: PlacementNode
  destroyed: string[]
  containers_unconfirmed: EvictionUnconfirmedContainer[]
}

export interface ExecResponse {
  exit_code: number
  stdout: string
  stderr: string
}

export interface ReadFileResponse {
  path: string
  contents_b64: string
  size: number
}

export interface CreateSandboxBody {
  name?: string
  /** Node name, id, or `control-plane`/`0`/`local`. Omit for default placement. */
  node?: string
  timeout_secs?: number
  cpu_limit?: number
  memory_limit_mb?: number
}

export interface ApiResult<T> {
  /** HTTP status, or `0` when the request never got a response (network error / client deadline). */
  status: number
  /** Parsed JSON body when the response had one, else `undefined`. */
  json: T | ProblemBody | undefined
  /** Raw body text (or the network/deadline error message when `status === 0`). */
  text: string
}

/** `<base>/api/v1/sandboxes<path>` — same layout as the CLI's `sandboxUrl`. */
export function sandboxApiUrl(baseUrl: string, path: string): string {
  return `${normalizeApiUrl(baseUrl)}/v1/sandboxes${path}`
}

/** Container label of a sandbox: its public id without the `sbx_` prefix. */
export function sandboxLabel(publicId: string): string {
  return publicId.startsWith(SANDBOX_PUBLIC_ID_PREFIX)
    ? publicId.slice(SANDBOX_PUBLIC_ID_PREFIX.length)
    : publicId
}

/** Docker container name of a sandbox on its node: `temps-sandbox-<label>`. */
export function sandboxContainerName(publicId: string): string {
  return `${SANDBOX_CONTAINER_PREFIX}${sandboxLabel(publicId)}`
}

/**
 * Where a worker's agent keeps a sandbox's work dir:
 * `<agent data dir>/sandboxes/<label>` (`AgentConfig::sandbox_work_root`).
 * The agent data dir is `$TEMPS_DATA_DIR` if set, else `~/.temps`; return
 * both so a scenario can locate the real one on the host instead of
 * hard-coding the role script's environment.
 */
export function workerWorkDirCandidates(publicId: string): string[] {
  const label = sandboxLabel(publicId)
  return [`/root/.temps/sandboxes/${label}`, `/var/lib/temps/sandboxes/${label}`]
}

/** Split `docker ps --format '{{.Names}}'` output into names. */
export function parseDockerNames(stdout: string): string[] {
  return stdout
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
}

/**
 * Sandbox-owned containers in a `docker ps` listing: the sandbox itself and
 * its per-sandbox egress proxy both start with `temps-sandbox-`. With
 * `publicId`, only that sandbox's containers (the proxy's name embeds the
 * sandbox container name).
 */
export function sandboxContainersIn(names: string[], publicId?: string): string[] {
  const owned = names.filter((name) => name.startsWith(SANDBOX_CONTAINER_PREFIX))
  if (publicId === undefined) return owned
  const container = sandboxContainerName(publicId)
  return owned.filter((name) => name === container || name.endsWith(`-${container}`))
}

/** Return the body as an RFC 7807 problem if it looks like one. */
export function parseProblem(json: unknown): ProblemBody | undefined {
  if (!json || typeof json !== 'object') return undefined
  const candidate = json as Record<string, unknown>
  if (
    typeof candidate.type !== 'string' &&
    typeof candidate.title !== 'string' &&
    typeof candidate.detail !== 'string'
  ) {
    return undefined
  }
  return {
    type: typeof candidate.type === 'string' ? candidate.type : undefined,
    title: typeof candidate.title === 'string' ? candidate.title : undefined,
    status: typeof candidate.status === 'number' ? candidate.status : undefined,
    detail: typeof candidate.detail === 'string' ? candidate.detail : undefined,
  }
}

/** One-line description of a result for error messages (body capped). */
export function describeResult(res: ApiResult<unknown>): string {
  const body = res.text.length > 400 ? `${res.text.slice(0, 400)}…` : res.text
  return res.status === 0 ? `no response (${body})` : `HTTP ${res.status}: ${body || '(empty body)'}`
}

/** Throw unless `res` has exactly `status`; returns the typed JSON body. */
export function expectStatus<T>(res: ApiResult<T>, status: number, what: string): T {
  if (res.status !== status) {
    throw new Error(`${what}: expected HTTP ${status}, got ${describeResult(res)}`)
  }
  return res.json as T
}

/**
 * Throw unless `res` is a problem response with `status` and one of `types`.
 * `detailIncludes` additionally requires the detail to name something (e.g.
 * the node), case-insensitively.
 */
export function expectProblem(
  res: ApiResult<unknown>,
  opts: { status: number; types?: readonly string[]; detailIncludes?: string },
  what: string,
): ProblemBody {
  if (res.status !== opts.status) {
    throw new Error(`${what}: expected HTTP ${opts.status}, got ${describeResult(res)}`)
  }
  const problem = parseProblem(res.json)
  if (!problem) {
    throw new Error(`${what}: HTTP ${res.status} without a problem+json body: ${describeResult(res)}`)
  }
  if (opts.types && !opts.types.includes(problem.type ?? '')) {
    throw new Error(
      `${what}: expected problem type ${opts.types.join(' or ')}, got ${problem.type ?? '(none)'} (${problem.title ?? ''}: ${problem.detail ?? ''})`,
    )
  }
  if (
    opts.detailIncludes !== undefined &&
    !`${problem.title ?? ''} ${problem.detail ?? ''}`.toLowerCase().includes(opts.detailIncludes.toLowerCase())
  ) {
    throw new Error(
      `${what}: expected the problem to mention '${opts.detailIncludes}', got: ${problem.title ?? ''}: ${problem.detail ?? ''}`,
    )
  }
  return problem
}

/** The placement row for a node id (control plane is id 0). */
export function findPlacementNode(placement: SandboxPlacement, nodeId: number): PlacementNode | undefined {
  return placement.nodes.find((node) => node.id === nodeId)
}

/** Problems with an eviction response; empty means it destroyed exactly what was expected, cleanly. */
export function evictionIssues(resp: NodeEvictionResponse, expectedIds: readonly string[]): string[] {
  const issues: string[] = []
  for (const id of expectedIds) {
    if (!resp.destroyed.includes(id)) issues.push(`sandbox ${id} missing from destroyed [${resp.destroyed.join(', ')}]`)
  }
  for (const entry of resp.containers_unconfirmed) {
    issues.push(`container of ${entry.sandbox_id} not confirmed removed: ${entry.reason} (cleanup: ${entry.cleanup_command})`)
  }
  return issues
}

/**
 * Parse `sh -c 'echo <marker>; hostname'` output: the marker must be the
 * first line; the hostname is the line after it.
 */
export function parseMarkerAndHostname(stdout: string, marker: string): { markerSeen: boolean; hostname: string } {
  const lines = parseDockerNames(stdout)
  const index = lines.indexOf(marker)
  return { markerSeen: index !== -1, hostname: index === -1 ? '' : (lines[index + 1] ?? '') }
}

/**
 * Whether ISO timestamp `candidate` is strictly later than `baseline`. An
 * empty or unparsable baseline means "no heartbeat seen yet", so any valid
 * candidate is newer; an empty or unparsable candidate never is.
 */
export function isNewerTimestamp(candidate: string, baseline: string): boolean {
  const next = Date.parse(candidate)
  if (Number.isNaN(next)) return false
  const previous = Date.parse(baseline)
  return Number.isNaN(previous) || next > previous
}

export function toBase64(text: string): string {
  return Buffer.from(text, 'utf8').toString('base64')
}

export function fromBase64(b64: string): string {
  return Buffer.from(b64, 'base64').toString('utf8')
}

/**
 * Published images to try, in order, when the exact sandbox image the
 * control plane asks for isn't in the registry (a build without a release
 * manifest asks for an unpublished version tag). The first one that pulls
 * is tagged with the exact name, which the provider then uses as-is instead
 * of building the image locally (15+ minutes inside Docker-in-Docker).
 * Only GHCR sandbox images get fallbacks: `repo:tag-beta`, then `repo:beta`.
 */
export function sandboxImageFallbacks(image: string): string[] {
  const at = image.lastIndexOf(':')
  if (at <= image.lastIndexOf('/')) return []
  const repo = image.slice(0, at)
  const tag = image.slice(at + 1)
  if (!repo.startsWith('ghcr.io/gotempsh/temps-sandbox-') || image.includes('@')) return []
  const candidates = tag.endsWith('-beta') ? [`${repo}:beta`] : [`${repo}:${tag}-beta`, `${repo}:beta`]
  return candidates.filter((c) => c !== image)
}

/** Default client-side deadline for a sandbox API call. */
const DEFAULT_TIMEOUT_MS = 120_000

/** Typed calls against `/api/v1/sandboxes/*` with explicit per-call deadlines. */
export class SandboxApi {
  constructor(private readonly cfg: TempsClientConfig) {}

  async request<T>(
    method: 'GET' | 'POST' | 'PUT' | 'DELETE',
    path: string,
    body?: unknown,
    timeoutMs = DEFAULT_TIMEOUT_MS,
  ): Promise<ApiResult<T>> {
    const headers: Record<string, string> = { Authorization: `Bearer ${this.cfg.apiKey}` }
    if (body !== undefined) headers['Content-Type'] = 'application/json'
    let response: Response
    try {
      response = await fetch(sandboxApiUrl(this.cfg.url, path), {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(timeoutMs),
      })
    } catch (error) {
      return { status: 0, json: undefined, text: `${method} ${path}: ${(error as Error).message}` }
    }
    const text = await response.text().catch((error: Error) => `(body unreadable: ${error.message})`)
    let json: T | ProblemBody | undefined
    if (text) {
      try {
        json = JSON.parse(text) as T
      } catch {
        json = undefined
      }
    }
    return { status: response.status, json, text }
  }

  create(body: CreateSandboxBody, timeoutMs?: number) {
    return this.request<SandboxResponse>('POST', '', body, timeoutMs)
  }

  get(id: string) {
    return this.request<SandboxResponse>('GET', `/${encodeURIComponent(id)}`)
  }

  exec(id: string, cmd: string[], timeoutMs?: number) {
    return this.request<ExecResponse>('POST', `/${encodeURIComponent(id)}/exec`, { cmd }, timeoutMs)
  }

  writeFile(id: string, path: string, contents: string) {
    return this.request<undefined>('POST', `/${encodeURIComponent(id)}/fs/write`, {
      path,
      contents_b64: toBase64(contents),
    })
  }

  readFile(id: string, path: string) {
    return this.request<ReadFileResponse>(
      'GET',
      `/${encodeURIComponent(id)}/fs/read?path=${encodeURIComponent(path)}`,
    )
  }

  /** Non-destructive stop: the container stops, the row and filesystem stay. */
  pause(id: string) {
    return this.request<SandboxResponse>('POST', `/${encodeURIComponent(id)}/pause`)
  }

  /** Start a paused sandbox's container again. */
  resume(id: string) {
    return this.request<SandboxResponse>('POST', `/${encodeURIComponent(id)}/resume`)
  }

  destroy(id: string, timeoutMs?: number) {
    return this.request<undefined>('POST', `/${encodeURIComponent(id)}/destroy`, undefined, timeoutMs)
  }

  snapshot(id: string, label: string) {
    return this.request<unknown>('POST', `/${encodeURIComponent(id)}/snapshots`, { label })
  }

  placement() {
    return this.request<SandboxPlacement>('GET', '/placement')
  }

  /** Always sends the `allowed_node_ids` member explicitly; `null` = every node. */
  setPlacement(allowedNodeIds: number[] | null) {
    return this.request<SandboxPlacement>('PUT', '/placement', { allowed_node_ids: allowedNodeIds })
  }

  evict(node: string, timeoutMs?: number) {
    return this.request<NodeEvictionResponse>(
      'POST',
      `/placement/nodes/${encodeURIComponent(node)}/evict`,
      undefined,
      timeoutMs,
    )
  }
}
