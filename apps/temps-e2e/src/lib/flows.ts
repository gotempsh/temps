/**
 * High-level Temps control-plane flows used by the e2e commands: create a
 * project, provision a database service, deploy a prebuilt image, poll for
 * health, resolve the public URL, and verify traffic via proxy logs.
 *
 * Each helper takes the per-call `client` so a run is fully isolated.
 */
import {
  createProject,
  deleteProject,
  getEnvironments,
  deployFromImage,
  getDeployment,
  triggerProjectPipeline,
  getLastDeployment,
  createService,
  deleteService,
  getProxyLogs,
  teardownDeployment,
  createDomain,
  finalizeOrder,
  deleteDomain,
  createCustomDomain,
  linkCustomDomainToCertificate,
  deleteCustomDomain,
  createEmailProvider,
  deleteEmailProvider,
  createEmailDomain,
  verifyDomain,
  deleteEmailDomain,
  sendEmail,
  getEmailLinks,
  getEmailEvents,
  kvStatus,
  kvEnable,
  blobStatus,
  blobEnable,
  createDnsProvider,
  addManagedDomain,
  verifyManagedDomain,
  setupDnsChallenge,
  removeManagedDomain,
  deleteDnsProvider,
} from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import tls, { type TLSSocket } from 'node:tls'
import { unwrap, normalizeApiUrl } from './client.ts'

/** Terminal-success deployment states (see ADR / generated DeploymentResponse.state). */
const DEPLOY_SUCCESS = new Set(['completed', 'succeeded', 'running', 'active'])
const DEPLOY_FAILED = new Set(['failed', 'cancelled', 'errored', 'error'])

/**
 * Shared with cli-exec-based flows (cli-scenario.ts) that poll
 * `deployments status --json` through the real CLI subprocess instead of the
 * SDK, so both paths classify the same DeploymentResponse.status strings
 * identically.
 */
export function isTerminalDeployStatus(status: string): { terminal: boolean; ok: boolean } {
  const s = status.toLowerCase()
  const ok = DEPLOY_SUCCESS.has(s)
  const failed = DEPLOY_FAILED.has(s)
  return { terminal: ok || failed, ok }
}

export interface CreatedProject {
  id: number
  name: string
  slug: string
}

/** Create a Dockerfile/image-based project tagged with the run id. */
export async function createE2eProject(
  client: Client,
  opts: { name: string; exposedPort?: number },
): Promise<CreatedProject> {
  const res = await createProject({
    client,
    body: {
      name: opts.name,
      directory: '.',
      main_branch: 'main',
      // 'dockerfile' preset is the generic container path; image deploys don't
      // need a git repo or build command.
      preset: 'dockerfile',
      storage_service_ids: [],
      source_type: 'docker_image',
      exposed_port: opts.exposedPort ?? 80,
      is_web_app: true,
    },
  })
  const p = unwrap(res, 'createProject')
  return { id: p.id, name: p.name, slug: p.slug }
}

/**
 * Create a real Git-backed project against a public repo -- `source_type`
 * defaults to `'git'` server-side. Note: creating a Git project always
 * auto-queues an initial deployment as a side effect, regardless of
 * `automatic_deploy` (that flag governs future git-push-triggered deploys,
 * not this one) -- `triggerPipelineAndGetDeploymentId` accounts for it.
 */
export async function createE2eGitProject(
  client: Client,
  opts: {
    name: string
    repoOwner: string
    repoName: string
    gitUrl: string
    directory: string
    preset: string
    mainBranch?: string
  },
): Promise<CreatedProject> {
  const res = await createProject({
    client,
    body: {
      name: opts.name,
      repo_owner: opts.repoOwner,
      repo_name: opts.repoName,
      git_url: opts.gitUrl,
      is_public_repo: true,
      directory: opts.directory,
      main_branch: opts.mainBranch ?? 'main',
      preset: opts.preset,
      storage_service_ids: [],
      automatic_deploy: false,
      is_web_app: true,
    },
  })
  const p = unwrap(res, 'createProject')
  return { id: p.id, name: p.name, slug: p.slug }
}

/**
 * Trigger a real build+deploy pipeline for a project (the same action a git
 * push to the tracked branch would cause) and return the id of the
 * deployment it created. `GET /projects/{id}/last-deployment` 404s until a
 * deployment row exists, so this polls through that instead of assuming the
 * trigger call itself returns one -- `TriggerPipelineResponse` doesn't carry
 * a deployment id.
 */
export async function triggerPipelineAndGetDeploymentId(
  client: Client,
  opts: { projectId: number; environmentId: number; branch?: string },
): Promise<number> {
  // Git-type projects auto-queue an initial deployment as a side effect of
  // `POST /projects` itself (`Queueing initial deployment job for Git
  // project` server-side) -- unconditionally, regardless of
  // `automatic_deploy`, and asynchronously (the row doesn't exist the
  // instant `createProject` returns). Wait for THAT one to actually land
  // and use its id as the baseline -- a single unpolled check races it: if
  // the auto-deployment's row is created after our check but before our
  // trigger call, it looks like "the deployment our trigger created" (its id
  // is > whatever we saw, possibly 0/none), and we'd end up watching it
  // instead. The platform correctly cancels/supersedes the auto-deployment
  // the moment our explicit trigger creates a newer one for the same
  // environment, so watching the wrong one surfaces as a spurious
  // "cancelled" failure.
  const baseline = await pollUntil(
    async () => {
      const res = await getLastDeployment({ client, path: { id: opts.projectId } })
      return res.data ?? null
    },
    (d) => d !== null,
    { timeoutMs: 15_000, intervalMs: 1000, label: 'the auto-queued initial deployment to land' },
  )
  const baselineId = baseline!.id

  unwrap(
    await triggerProjectPipeline({
      client,
      path: { id: opts.projectId },
      body: { environment_id: opts.environmentId, branch: opts.branch ?? null, tag: null, commit: null },
    }),
    'triggerProjectPipeline',
  )
  const deployment = await pollUntil(
    async () => {
      const res = await getLastDeployment({ client, path: { id: opts.projectId } })
      return res.data ?? null
    },
    (d) => d !== null && d.id > baselineId,
    { timeoutMs: 30_000, intervalMs: 2000, label: 'a NEW deployment row (id > baseline) to appear after trigger-pipeline' },
  )
  return deployment!.id
}

/** Pick the production (non-preview) environment for a project. */
export async function getProductionEnvironment(
  client: Client,
  projectId: number,
): Promise<{ id: number; mainUrl: string; name: string; slug: string }> {
  const res = await getEnvironments({ client, path: { project_id: projectId } })
  const envs = unwrap(res, 'getEnvironments')
  const prod = envs.find((e) => e.is_preview === false) ?? envs[0]
  if (!prod) throw new Error(`No environment found for project ${projectId}`)
  return { id: prod.id, mainUrl: prod.main_url, name: prod.name, slug: prod.slug }
}

/** Trigger a deploy from a prebuilt public image; returns the deployment id. */
export async function deployImage(
  client: Client,
  opts: { projectId: number; environmentId: number; imageRef: string },
): Promise<number> {
  const res = await deployFromImage({
    client,
    path: { project_id: opts.projectId, environment_id: opts.environmentId },
    body: { image_ref: opts.imageRef },
  })
  const d = unwrap(res, 'deployFromImage')
  return d.id
}

export interface DeployStatus {
  state: string
  url?: string | null
  terminal: boolean
  ok: boolean
}

/** Read a deployment's current state. */
export async function getDeployStatus(
  client: Client,
  projectId: number,
  deploymentId: number,
): Promise<DeployStatus> {
  const res = await getDeployment({
    client,
    path: { project_id: projectId, deployment_id: deploymentId },
  })
  const d = unwrap(res, 'getDeployment') as { state?: string; status?: string; url?: string | null }
  const state = (d.state ?? d.status ?? 'unknown').toLowerCase()
  const ok = DEPLOY_SUCCESS.has(state)
  const failed = DEPLOY_FAILED.has(state)
  return { state, url: d.url, terminal: ok || failed, ok }
}

/** Poll a deployment until it reaches a terminal state or times out. */
export async function waitForDeployment(
  client: Client,
  opts: {
    projectId: number
    deploymentId: number
    timeoutMs?: number
    intervalMs?: number
    onPoll?: (s: DeployStatus, elapsedMs: number) => void
  },
): Promise<DeployStatus> {
  const timeoutMs = opts.timeoutMs ?? 5 * 60_000
  const intervalMs = opts.intervalMs ?? 3000
  const start = performance.now()
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const s = await getDeployStatus(client, opts.projectId, opts.deploymentId)
    const elapsed = performance.now() - start
    opts.onPoll?.(s, elapsed)
    if (s.terminal) return s
    if (elapsed > timeoutMs) {
      throw new Error(
        `Deployment ${opts.deploymentId} did not reach a terminal state within ${Math.round(timeoutMs / 1000)}s (last state: ${s.state})`,
      )
    }
    await sleep(intervalMs)
  }
}

export interface CreatedService {
  id: number
  name: string
}

/** Provision an external service (postgres/redis/etc.) tagged with the run id. */
export async function createE2eService(
  client: Client,
  opts: {
    name: string
    serviceType: 'postgres' | 'redis' | 'mongodb' | 's3' | 'kv' | 'blob' | 'rustfs' | 'minio'
    version?: string
    parameters?: Record<string, unknown>
  },
): Promise<CreatedService> {
  const res = await createService({
    client,
    body: {
      name: opts.name,
      service_type: opts.serviceType,
      parameters: opts.parameters ?? {},
      ...(opts.version ? { version: opts.version } : {}),
    },
  })
  const s = unwrap(res, 'createService') as { id: number; name: string }
  return { id: s.id, name: s.name }
}

/**
 * Provision an HA cluster service (currently only `postgres` supports
 * `topology: 'cluster'` -- see `CLUSTER_SERVICE_TYPES` in
 * `web/src/pages/CreateServiceNew.tsx`). Cluster creation is asynchronous
 * server-side (`ExternalServiceManager::create_service` spawns a background
 * task and returns immediately with `status: "creating"`) -- callers must
 * poll `getService` for `status === 'running'` themselves, same as the
 * console does.
 *
 * All members are placed on the control plane (`node_id: null`) --
 * multi-container HA on a single Docker host, matching how
 * `PostgresClusterService` names/ports members (`postgres-<name>-monitor`,
 * `postgres-<name>-<ordinal>`, unique host ports per member) -- distinct
 * from multi-node/WireGuard clustering, which needs real separate hosts.
 * Per `DEFAULT_CLUSTER_ROLES` in the console, every data node is requested
 * as `role: 'replica'`; pg_auto_failover elects whichever registers first
 * as the actual primary (see `live_state` on `ServiceMemberInfo`), the
 * client never picks one.
 */
export async function createE2eClusterService(
  client: Client,
  opts: {
    name: string
    serviceType: 'postgres'
    dataNodes: number
    parameters?: Record<string, unknown>
  },
): Promise<CreatedService> {
  const members = [
    { role: 'monitor', node_id: null },
    ...Array.from({ length: opts.dataNodes }, () => ({ role: 'replica', node_id: null })),
  ]
  const res = await createService({
    client,
    body: {
      name: opts.name,
      service_type: opts.serviceType,
      parameters: opts.parameters ?? {},
      topology: 'cluster',
      members,
    },
  })
  const s = unwrap(res, 'createService') as { id: number; name: string }
  return { id: s.id, name: s.name }
}

/**
 * Count proxy-log entries recorded for a host since a given time. Used to verify
 * that generated traffic actually landed on the proxy/ingest pipeline.
 */
export async function countProxyLogs(
  client: Client,
  opts: { host?: string; sinceMs?: number },
): Promise<number> {
  const query: Record<string, unknown> = { page: 1, page_size: 1 }
  if (opts.host) query.host = opts.host
  if (opts.sinceMs) query.start_date = new Date(opts.sinceMs).toISOString()
  const res = await getProxyLogs({ client, query })
  const data = unwrap(res, 'getProxyLogs') as { total?: number; logs?: unknown[] }
  return data.total ?? data.logs?.length ?? 0
}

/**
 * Best-effort teardown of resources created by a run. Never throws.
 * Order matters: tear down running deployments (stops the container + removes
 * the proxy route) BEFORE deleting the project row, otherwise the container can
 * linger serving traffic after the project is gone.
 */
export async function teardown(
  client: Client,
  resources: {
    deployments?: { projectId: number; deploymentId: number }[]
    projectIds?: number[]
    serviceIds?: number[]
  },
): Promise<{
  teardownDeployments: number
  deletedProjects: number
  deletedServices: number
  errors: string[]
}> {
  const errors: string[] = []
  let teardownDeployments = 0
  let deletedProjects = 0
  let deletedServices = 0

  for (const d of resources.deployments ?? []) {
    try {
      await teardownDeployment({
        client,
        path: { project_id: d.projectId, deployment_id: d.deploymentId },
      })
      teardownDeployments++
    } catch (e) {
      errors.push(`teardownDeployment(${d.deploymentId}): ${(e as Error).message}`)
    }
  }
  // teardownDeployment returns before the container is fully removed (the stop
  // is async server-side). Give it a short grace so deleting the project below
  // doesn't orphan a still-stopping container.
  if ((resources.deployments ?? []).length > 0) {
    await sleep(3000)
  }
  for (const id of resources.projectIds ?? []) {
    try {
      await deleteProject({ client, path: { id } })
      deletedProjects++
    } catch (e) {
      errors.push(`deleteProject(${id}): ${(e as Error).message}`)
    }
  }
  for (const id of resources.serviceIds ?? []) {
    try {
      await deleteService({ client, path: { id } })
      deletedServices++
    } catch (e) {
      errors.push(`deleteService(${id}): ${(e as Error).message}`)
    }
  }
  return { teardownDeployments, deletedProjects, deletedServices, errors }
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

/** Poll `fn` until `predicate` passes or `timeoutMs` elapses; logs each observed value via `onPoll`. */
export async function pollUntil<T>(
  fn: () => Promise<T>,
  predicate: (v: T) => boolean,
  opts: { timeoutMs: number; intervalMs?: number; onPoll?: (v: T, elapsedMs: number) => void; label: string },
): Promise<T> {
  const intervalMs = opts.intervalMs ?? 3000
  const start = performance.now()
  let last: T | undefined
  while (performance.now() - start < opts.timeoutMs) {
    last = await fn()
    opts.onPoll?.(last, performance.now() - start)
    if (predicate(last)) return last
    await sleep(intervalMs)
  }
  throw new Error(
    `${opts.label} did not converge within ${Math.round(opts.timeoutMs / 1000)}s (last: ${JSON.stringify(last)})`,
  )
}

/**
 * Turn a deployment's public `main_url` into a target the load generator can
 * actually reach. The proxy routes by Host header, so we send requests to the
 * INSTANCE's address (e.g. http://localhost:8080) while passing the app's
 * hostname as the Host header. This avoids depending on the app's scheme/port
 * (the main_url is often https on :443) and on external DNS — it always lands on
 * the same proxy that serves real traffic.
 */
export function resolveLoadTarget(
  instanceUrl: string,
  appMainUrl: string,
): { url: string; host: string; logHost: string; headers: Record<string, string> } {
  const parsed = new URL(appMainUrl)
  // `host` includes the port (e.g. `app.sslip.io:8080`) — this is what the proxy
  // routes on, so it's the Host header we send.
  const appHost = parsed.host
  // The proxy records the bare HOSTNAME (no port) in `proxy_logs.host`, so the
  // verification query must match on hostname-only — otherwise a host carrying a
  // non-standard port (the common local case) never matches and traffic looks
  // "unrecorded" even though it landed.
  const logHost = parsed.hostname
  // Strip any /api suffix the instance URL might carry; the proxy data plane is
  // the bare origin.
  const base = instanceUrl.replace(/\/+$/, '').replace(/\/api$/, '')
  const baseUrl = new URL(base)
  // Hit the proxy origin with the app's host header.
  const url = `${baseUrl.protocol}//${baseUrl.host}/`
  return { url, host: appHost, logHost, headers: { Host: appHost } }
}

/**
 * Poll an HTTP target until it returns a non-5xx/non-0 response or times out.
 * Confirms the deployed app is actually serving before we load-test it (the
 * deployment status string alone is not a reliability signal).
 */
export async function waitForHttpReady(opts: {
  url: string
  headers?: Record<string, string>
  timeoutMs?: number
  intervalMs?: number
  onPoll?: (status: number, elapsedMs: number) => void
}): Promise<number> {
  const timeoutMs = opts.timeoutMs ?? 120_000
  const intervalMs = opts.intervalMs ?? 2000
  const start = performance.now()
  let last = 0
  while (performance.now() - start < timeoutMs) {
    try {
      const ctrl = new AbortController()
      const t = setTimeout(() => ctrl.abort(), 10_000)
      const res = await fetch(opts.url, { headers: opts.headers, signal: ctrl.signal })
      clearTimeout(t)
      await res.arrayBuffer().catch(() => undefined)
      last = res.status
      opts.onPoll?.(res.status, performance.now() - start)
      if (res.status > 0 && res.status < 500) return res.status
    } catch {
      opts.onPoll?.(0, performance.now() - start)
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `App did not become HTTP-ready within ${Math.round(timeoutMs / 1000)}s (last status: ${last})`,
  )
}

/**
 * True when a response body is the Temps console SPA shell, not an app.
 *
 * Must match ONLY the console — a deployed React/Vite app is also an HTML
 * document with a `<div id="root">`, so matching on that generic marker gives
 * false positives. Key on the console's own page <title> (and its favicon path),
 * which a deployed example never sets.
 */
export function looksLikeConsoleFallback(body: string): boolean {
  if (!/<!doctype html>/i.test(body)) return false
  // The Temps console index.html sets `<title>Temps</title>` in production
  // builds and `<title>Temps - Development Build</title>` in dev builds.
  // A deployed app never starts its page title with "Temps" (they use their
  // own names), so matching the prefix is sufficient and avoids brittleness
  // against the exact suffix used in each build mode.
  //
  // Character class is `[\s<]` (space or `<`), NOT `[\s\-<]`. In both real
  // title forms the character immediately after "Temps" is either `<` (closing
  // tag, production) or ` ` (space before " - Development Build", dev). A
  // hyphen directly after "Temps" is never produced by any Temps build, and
  // including `\-` in the class would cause false positives for any deployed
  // app whose title starts with "Temps-" (e.g. "Temps-sync-dashboard").
  return /<title>\s*Temps[\s<]/i.test(body)
}

/**
 * Poll the target until it serves the deployed app rather than the Temps console
 * SPA fallback, then return. The proxy serves the console shell (HTTP 200,
 * `<title>Temps</title>`) for unknown/failed hosts AND for the brief window
 * before a freshly-started container's route propagates — so a bare status check
 * cannot distinguish "app is serving" from "deploy failed / not routed yet".
 * This is the guard that makes a broken deploy actually fail (after giving the
 * route a fair chance to propagate).
 */
export async function assertNotConsoleFallback(opts: {
  url: string
  headers?: Record<string, string>
  timeoutMs?: number
  intervalMs?: number
}): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 30_000
  const intervalMs = opts.intervalMs ?? 1500
  const start = performance.now()
  let body = ''
  let status = 0
  while (performance.now() - start < timeoutMs) {
    const ctrl = new AbortController()
    const t = setTimeout(() => ctrl.abort(), 10_000)
    try {
      const res = await fetch(opts.url, { headers: opts.headers, signal: ctrl.signal })
      status = res.status
      body = await res.text()
      if (!looksLikeConsoleFallback(body)) return // real app response
    } catch {
      // transient — retry
    } finally {
      clearTimeout(t)
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `served the Temps console fallback (HTTP ${status}) instead of the app after ` +
      `${Math.round(timeoutMs / 1000)}s — the deployment is not actually serving ` +
      `(body starts: ${JSON.stringify(body.slice(0, 80))})`,
  )
}

/**
 * Fetch a target and return its raw status/body. Used where a scenario needs
 * to assert on the EXACT response body (e.g. "traffic is serving version A's
 * distinguishable text, byte for byte") rather than just "not the console
 * fallback".
 */
export async function fetchBody(
  url: string,
  headers?: Record<string, string>,
  timeoutMs = 10_000,
): Promise<{ status: number; body: string }> {
  const ctrl = new AbortController()
  const t = setTimeout(() => ctrl.abort(), timeoutMs)
  try {
    const res = await fetch(url, { headers, signal: ctrl.signal })
    const body = await res.text()
    return { status: res.status, body }
  } finally {
    clearTimeout(t)
  }
}

/**
 * The mirror of `assertNotConsoleFallback`: poll a target until it DOES serve
 * the Temps console SPA fallback (or times out still serving something else).
 *
 * Used to prove a deployment pause actually took live traffic offline: after
 * pausing, `route_table::load_routes` finds no routable container for the
 * environment (see the fix note there) and skips the route entirely, so the
 * proxy falls through to its unknown-host console fallback. A real app
 * response (or a hung/refused connection) here means pause did NOT actually
 * stop traffic.
 */
export async function waitForConsoleFallback(opts: {
  url: string
  headers?: Record<string, string>
  timeoutMs?: number
  intervalMs?: number
}): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 30_000
  const intervalMs = opts.intervalMs ?? 1500
  const start = performance.now()
  let body = ''
  let status = 0
  while (performance.now() - start < timeoutMs) {
    try {
      const r = await fetchBody(opts.url, opts.headers, 10_000)
      status = r.status
      body = r.body
      if (looksLikeConsoleFallback(body)) return
    } catch {
      // transient — retry
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `expected the paused deployment to serve the Temps console fallback, but got HTTP ${status} ` +
      `with a non-fallback body after ${Math.round(timeoutMs / 1000)}s ` +
      `(body starts: ${JSON.stringify(body.slice(0, 80))})`,
  )
}

/**
 * A run id derived from a timestamp passed in by the caller, plus a random
 * suffix.
 *
 * The timestamp alone collides when two scenario runs start within the same
 * millisecond — realistic whenever more than one `scenario`/`examples`
 * process is launched concurrently (parallel CI shards, a dev running two
 * terminals). The random suffix makes each call unique regardless of timing;
 * the timestamp prefix is kept so ids still sort roughly chronologically for
 * debugging.
 */
export function makeRunId(nowMs: number): string {
  const rand = Math.random().toString(36).slice(2, 8)
  return `e2e-${nowMs.toString(36)}-${rand}`
}

/**
 * Registers the host machine's IP with pebble-challtestsrv's mock-DNS
 * management API so any hostname Pebble tries to resolve (the throwaway
 * `tls-scenario` test domain) points back at wherever the target `temps
 * serve` instance's HTTP-01 responder is actually listening.
 *
 * Pebble and pebble-challtestsrv run in Docker (`docker-compose.e2e.yml`)
 * while `temps serve` runs natively on the host, so `host.docker.internal`
 * -- Docker's own name for "the host machine" -- has to be resolved from
 * INSIDE a container on that compose network (the host process itself can't
 * resolve that name; it's only present in Docker's embedded DNS) before it
 * can be handed to challtestsrv's `/set-default-ipv4`.
 *
 * Also clears the default IPv6 (AAAA) answer. challtestsrv answers AAAA
 * queries with `::1` unless told otherwise, and Go's dialer (used by both
 * Pebble and validators generally) prefers a AAAA result when one exists —
 * so leaving it set makes every validation attempt dial `[::1]:5002` inside
 * Pebble's own container (nothing listens there) instead of the host,
 * regardless of what the A record says.
 */
export async function registerPebbleDefaultIp(opts?: {
  network?: string
  managementUrl?: string
}): Promise<string> {
  const network = opts?.network ?? 'temps-e2e-pebble-net'
  const managementUrl = opts?.managementUrl ?? 'http://localhost:8056'

  const proc = Bun.spawn(
    [
      'docker',
      'run',
      '--rm',
      '--network',
      network,
      'alpine:latest',
      'getent',
      'hosts',
      'host.docker.internal',
    ],
    { stdout: 'pipe', stderr: 'pipe' },
  )
  const [out, err, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (code !== 0) {
    throw new Error(`failed to resolve host.docker.internal via throwaway container: ${err}`)
  }
  const ip = out.trim().split(/\s+/)[0]
  if (!ip) {
    throw new Error(`could not parse host IP from getent output: ${JSON.stringify(out)}`)
  }

  const res = await fetch(`${managementUrl}/set-default-ipv4`, {
    method: 'POST',
    body: JSON.stringify({ ip }),
  })
  if (!res.ok) {
    throw new Error(`pebble-challtestsrv /set-default-ipv4 failed: HTTP ${res.status} ${await res.text()}`)
  }

  const ipv6Res = await fetch(`${managementUrl}/set-default-ipv6`, {
    method: 'POST',
    body: JSON.stringify({ ip: '' }),
  })
  if (!ipv6Res.ok) {
    throw new Error(
      `pebble-challtestsrv /set-default-ipv6 (clear) failed: HTTP ${ipv6Res.status} ${await ipv6Res.text()}`,
    )
  }

  return ip
}

export interface TlsDomainResources {
  /** The `domains` table row id — the standalone TLS-certificate record. */
  certificateId: number
  /** The `project_custom_domains` row id — the proxy-routing record. */
  customDomainId: number
  domain: string
}

/**
 * Create the two DB rows a working custom HTTPS domain needs: a standalone
 * TLS-certificate record (`POST /domains`, HTTP-01) and a project-routing
 * record (`POST /projects/{id}/custom-domains`) linked to it. These are
 * deliberately separate tables in temps (a certificate is not owned by any
 * one project) so both are required before the proxy will route AND serve
 * TLS for `opts.domain`.
 */
export async function setupTlsDomain(
  client: Client,
  opts: { domain: string; projectId: number; environmentId: number },
): Promise<TlsDomainResources> {
  const cert = unwrap(
    await createDomain({ client, body: { domain: opts.domain, challenge_type: 'http-01' } }),
    'createDomain',
  )

  const route = unwrap(
    await createCustomDomain({
      client,
      path: { project_id: opts.projectId },
      body: { domain: opts.domain, environment_id: opts.environmentId },
    }),
    'createCustomDomain',
  )

  unwrap(
    await linkCustomDomainToCertificate({
      client,
      path: { project_id: opts.projectId, domain_id: route.id, certificate_id: cert.id },
    }),
    'linkCustomDomainToCertificate',
  )

  return { certificateId: cert.id, customDomainId: route.id, domain: opts.domain }
}

export type TlsProvisionResult =
  | { ok: true; certificatePem: string }
  | { ok: false; reason: string }

/**
 * Complete ACME HTTP-01 issuance for a domain already created via
 * `setupTlsDomain`.
 *
 * This deliberately calls `POST /domains/{id}/order/finalize`, NOT
 * `POST /domains/{domain}/provision`. `createDomain` already auto-requests
 * the challenge (`DomainService::request_challenge`), which both (a) writes
 * the HTTP-01 token/key-authorization onto the `domains` row the proxy
 * serves `/.well-known/acme-challenge/` from, and (b) saves the ACME order
 * `finalize_order` completes. `provision_domain`'s HTTP-01 branch instead
 * calls `TlsService::initiate_http_challenge`, which unconditionally opens a
 * BRAND NEW, disconnected ACME order every call and returns
 * `ManualActionRequired` — for HTTP-01 (unlike its own DNS-01 branch, and
 * unlike this endpoint) it never actually completes one. `finalize_order`
 * is the one HTTP-01 completion path actually reachable from the API.
 */
export async function finalizeTlsCertificate(
  client: Client,
  certificateId: number,
): Promise<TlsProvisionResult> {
  const res = unwrap(
    await finalizeOrder({ client, path: { domain_id: certificateId } }),
    'finalizeOrder',
  )
  if (res.status !== 'active' || !res.certificate) {
    return {
      ok: false,
      reason: `order finalized but domain status is "${res.status}" (expected "active" with a certificate)`,
    }
  }
  return { ok: true, certificatePem: res.certificate }
}

/** Best-effort teardown of the two rows `setupTlsDomain` created. Never throws. */
export async function teardownTlsDomain(
  client: Client,
  opts: { projectId: number; customDomainId?: number; domain?: string },
): Promise<string[]> {
  const errors: string[] = []
  if (opts.customDomainId) {
    try {
      await deleteCustomDomain({
        client,
        path: { project_id: opts.projectId, domain_id: opts.customDomainId },
      })
    } catch (e) {
      errors.push(`deleteCustomDomain(${opts.customDomainId}): ${(e as Error).message}`)
    }
  }
  if (opts.domain) {
    try {
      await deleteDomain({ client, path: { domain: opts.domain } })
    } catch (e) {
      errors.push(`deleteDomain(${opts.domain}): ${(e as Error).message}`)
    }
  }
  return errors
}

/**
 * Register a Pebble-backed DNS provider (`PebbleDnsProvider` --
 * LOCAL DEV/TEST ONLY, gated by `TEMPS_ALLOW_PEBBLE_PROVIDER=1` and an
 * inverted SSRF guard requiring `managementUrl` to be loopback/private) and
 * add + verify a managed zone for it. Verification against Pebble always
 * succeeds -- `PebbleDnsProvider::get_zone` unconditionally returns `Some`,
 * unlike a real provider's zone lookup -- so this never blocks on real DNS
 * propagation or external credentials.
 */
export async function setupPebbleDnsZone(
  client: Client,
  opts: { zone: string; managementUrl?: string; name?: string },
): Promise<{ dnsProviderId: number }> {
  const provider = unwrap(
    await createDnsProvider({
      client,
      body: {
        name: opts.name ?? `e2e-pebble-${opts.zone}`,
        provider_type: 'pebble',
        credentials: { type: 'pebble', management_url: opts.managementUrl ?? 'http://localhost:8056' },
        description: 'temps-e2e dns01-wildcard-scenario (local test only)',
      },
    }),
    'createDnsProvider',
  )

  unwrap(
    await addManagedDomain({
      client,
      path: { id: provider.id },
      body: { domain: opts.zone, auto_manage: true },
    }),
    'addManagedDomain',
  )

  unwrap(
    await verifyManagedDomain({ client, path: { provider_id: provider.id, domain: opts.zone } }),
    'verifyManagedDomain',
  )

  return { dnsProviderId: provider.id }
}

export interface Dns01SetupResult {
  domainId: number
  recordsCreated: number
  totalRecords: number
}

/**
 * Create a wildcard domain with a `dns-01` challenge (`createDomain` already
 * auto-requests the ACME order via `DomainService::request_challenge`, same
 * as the HTTP-01 path) and push its TXT record(s) to the linked Pebble
 * provider. `setup-dns` resolves the record values from the just-created
 * order and calls `PebbleDnsProvider::create_record`, which POSTs to
 * pebble-challtestsrv's `/set-txt` -- no DNS state needs to be pre-seeded by
 * the test itself.
 */
export async function setupDns01WildcardDomain(
  client: Client,
  opts: { wildcardDomain: string; dnsProviderId: number },
): Promise<Dns01SetupResult> {
  const domain = unwrap(
    await createDomain({ client, body: { domain: opts.wildcardDomain, challenge_type: 'dns-01' } }),
    'createDomain',
  )

  const setup = unwrap(
    await setupDnsChallenge({
      client,
      path: { domain_id: domain.id },
      body: { dns_provider_id: opts.dnsProviderId },
    }),
    'setupDnsChallenge',
  )

  return { domainId: domain.id, recordsCreated: setup.records_created, totalRecords: setup.total_records }
}

/**
 * Best-effort teardown of everything `setupPebbleDnsZone` +
 * `setupDns01WildcardDomain` created. Never throws.
 */
export async function teardownDns01Resources(
  client: Client,
  opts: { wildcardDomain?: string; dnsProviderId?: number; zone?: string },
): Promise<string[]> {
  const errors: string[] = []
  if (opts.wildcardDomain) {
    try {
      await deleteDomain({ client, path: { domain: opts.wildcardDomain } })
    } catch (e) {
      errors.push(`deleteDomain(${opts.wildcardDomain}): ${(e as Error).message}`)
    }
  }
  if (opts.dnsProviderId && opts.zone) {
    try {
      await removeManagedDomain({ client, path: { provider_id: opts.dnsProviderId, domain: opts.zone } })
    } catch (e) {
      errors.push(`removeManagedDomain(${opts.zone}): ${(e as Error).message}`)
    }
  }
  if (opts.dnsProviderId) {
    try {
      await deleteDnsProvider({ client, path: { id: opts.dnsProviderId } })
    } catch (e) {
      errors.push(`deleteDnsProvider(${opts.dnsProviderId}): ${(e as Error).message}`)
    }
  }
  return errors
}

export interface TlsFetchResult {
  status: number
  body: string
  /** Peer certificate issuer fields (e.g. `{ CN: 'Pebble Intermediate CA ...' }`). */
  issuer: Record<string, string>
}

/**
 * Issue an HTTPS request against a specific address/port while sending a
 * chosen SNI + Host (independent of the connection address) — needed because
 * the target domain doesn't really resolve anywhere; we're dialing the proxy
 * directly and asking it, via SNI, to serve the cert + route for `opts.sni`.
 * `rejectUnauthorized: false` is required and safe here: the whole point of
 * this call is to inspect Pebble's self-signed test-root-issued certificate,
 * which no real trust store recognizes.
 *
 * Uses a raw `tls.connect` socket with a hand-rolled HTTP/1.1 request rather
 * than `node:https` — under Bun, `https.request`'s `response.socket` does
 * not implement `getPeerCertificate()` (throws "is not a function"), while a
 * directly created `tls.TLSSocket` does.
 *
 * The issuer is captured in the connect callback (handshake-complete time),
 * NOT after the response finishes: under Bun, `getPeerCertificate()` called
 * from the `'end'` handler returns `{}` (session details are already gone by
 * then), even though the exact same call made right after the handshake — or
 * under real Node.js at either point — returns the full certificate.
 */
export function fetchOverTls(opts: {
  host: string
  port: number
  sni: string
  path?: string
  timeoutMs?: number
}): Promise<TlsFetchResult> {
  return new Promise((resolve, reject) => {
    const timeoutMs = opts.timeoutMs ?? 15_000
    let issuer: Record<string, string> = {}
    const socket: TLSSocket = tls.connect(
      {
        host: opts.host,
        port: opts.port,
        servername: opts.sni,
        rejectUnauthorized: false,
      },
      () => {
        issuer = (socket.getPeerCertificate().issuer ?? {}) as Record<string, string>
        socket.write(
          `GET ${opts.path ?? '/'} HTTP/1.1\r\nHost: ${opts.sni}\r\nConnection: close\r\n\r\n`,
        )
      },
    )
    socket.setTimeout(timeoutMs, () => socket.destroy(new Error('TLS request timed out')))

    const chunks: Buffer[] = []
    socket.on('data', (c: Buffer) => chunks.push(c))
    socket.on('error', reject)
    socket.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8')
      const headerEnd = raw.indexOf('\r\n\r\n')
      const head = headerEnd === -1 ? raw : raw.slice(0, headerEnd)
      const body = headerEnd === -1 ? '' : raw.slice(headerEnd + 4)
      const statusMatch = head.match(/^HTTP\/1\.[01] (\d+)/)
      resolve({
        status: statusMatch ? Number(statusMatch[1]) : 0,
        body,
        issuer,
      })
    })
  })
}

/**
 * Poll `fetchOverTls` until the target serves the real app rather than the
 * Temps console SPA fallback. Route-table changes (a freshly created custom
 * domain) propagate via async PG NOTIFY (`temps-routes`), so the very next
 * request after creating one can still land on the fallback for a short
 * window — the same reason `assertNotConsoleFallback` (plain-HTTP scenarios)
 * retries instead of checking once.
 */
export async function waitForTlsAppReady(
  opts: { host: string; port: number; sni: string; path?: string },
  pollOpts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<TlsFetchResult> {
  const timeoutMs = pollOpts.timeoutMs ?? 30_000
  const intervalMs = pollOpts.intervalMs ?? 1500
  const start = performance.now()
  let last: TlsFetchResult = { status: 0, body: '', issuer: {} }
  while (performance.now() - start < timeoutMs) {
    last = await fetchOverTls(opts)
    if (last.status > 0 && last.status < 500 && !looksLikeConsoleFallback(last.body)) {
      return last
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `served the Temps console fallback (HTTP ${last.status}) instead of the app after ` +
      `${Math.round(timeoutMs / 1000)}s (body starts: ${JSON.stringify(last.body.slice(0, 80))})`,
  )
}

export interface CreatedEmailProvider {
  id: number
  name: string
}

/** Create an SMTP email provider pointed at a local test SMTP catcher (e.g. Mailpit). */
export async function createSmtpProvider(
  client: Client,
  opts: { name: string; host: string; port: number },
): Promise<CreatedEmailProvider> {
  const res = unwrap(
    await createEmailProvider({
      client,
      body: {
        name: opts.name,
        provider_type: 'smtp',
        region: 'local',
        smtp_credentials: { host: opts.host, port: opts.port, encryption: 'none' },
      },
    }),
    'createEmailProvider',
  )
  return { id: res.id, name: res.name }
}

export interface CreatedEmailDomain {
  id: number
  domain: string
}

/**
 * Register + verify an email-sending domain against an SMTP provider.
 * `SmtpProvider::verify_identity` (temps-email) has no upstream identity or
 * DNS record to check and always reports verified immediately — unlike
 * SES/Scaleway, there's no real DNS propagation wait to poll for here.
 */
export async function createVerifiedEmailDomain(
  client: Client,
  opts: { domain: string; providerId: number },
): Promise<CreatedEmailDomain> {
  const created = unwrap(
    await createEmailDomain({ client, body: { domain: opts.domain, provider_id: opts.providerId } }),
    'createEmailDomain',
  )
  const verified = unwrap(
    await verifyDomain({ client, path: { id: created.domain.id } }),
    'verifyDomain',
  )
  if (verified.domain.status !== 'verified') {
    throw new Error(
      `email domain ${opts.domain} did not verify (status: ${verified.domain.status})`,
    )
  }
  return { id: verified.domain.id, domain: verified.domain.domain }
}

export interface SentTrackedEmail {
  emailId: string
  linkIndex: number
}

/**
 * Send a real email with open + click tracking enabled through the full
 * provider chain (not the `/email-providers/{id}/test` shortcut, which
 * bypasses domain lookup entirely). Asserts the send actually went out
 * rather than silently landing in temps' own "no verified domain/provider"
 * capture fallback (`status: "captured"`), which returns 201 either way.
 */
export async function sendTrackedEmail(
  client: Client,
  opts: { from: string; to: string; subject: string; linkUrl: string },
): Promise<SentTrackedEmail> {
  const res = unwrap(
    await sendEmail({
      client,
      body: {
        from: opts.from,
        to: [opts.to],
        subject: opts.subject,
        html: `<p>e2e test email.</p><p><a href="${opts.linkUrl}">click here</a></p>`,
        track_opens: true,
        track_clicks: true,
      },
    }),
    'sendEmail',
  )
  if (res.status !== 'sent') {
    throw new Error(`email was not actually sent (status: "${res.status}") — provider/domain not reached`)
  }

  const links = unwrap(await getEmailLinks({ client, path: { id: res.id } }), 'getEmailLinks')
  const link = links[0]
  if (!link) {
    throw new Error(`email ${res.id} has no tracked links (expected exactly one from the HTML body)`)
  }
  return { emailId: res.id, linkIndex: link.link_index }
}

/**
 * Simulate a recipient's mail client loading the open-tracking pixel /
 * clicking a tracked link. These are deliberately unauthenticated GETs (real
 * mail clients never carry a Temps bearer token), so they're issued as plain
 * `fetch` calls against the instance's public base URL rather than through
 * the SDK client.
 */
export async function hitOpenTrackingPixel(instanceUrl: string, emailId: string): Promise<number> {
  const res = await fetch(`${normalizeApiUrl(instanceUrl)}/emails/${emailId}/track/open`)
  return res.status
}

export async function hitClickTrackingLink(
  instanceUrl: string,
  emailId: string,
  linkIndex: number,
): Promise<number> {
  const res = await fetch(
    `${normalizeApiUrl(instanceUrl)}/emails/${emailId}/track/click/${linkIndex}`,
    { redirect: 'manual' },
  )
  return res.status
}

export interface MailpitMessage {
  id: string
  from: string
  to: string[]
  subject: string
}

/** Query a Mailpit instance's REST API for a message matching `subject`. Never throws on "not found" — returns undefined so callers can poll. */
export async function findMailpitMessageBySubject(
  mailpitUrl: string,
  subject: string,
): Promise<MailpitMessage | undefined> {
  const res = await fetch(`${mailpitUrl.replace(/\/+$/, '')}/api/v1/messages?limit=50`)
  if (!res.ok) {
    throw new Error(`mailpit /api/v1/messages failed: HTTP ${res.status}`)
  }
  const data = (await res.json()) as {
    messages: Array<{
      ID: string
      From: { Address: string }
      To: Array<{ Address: string }>
      Subject: string
    }>
  }
  const match = data.messages.find((m) => m.Subject === subject)
  if (!match) return undefined
  return {
    id: match.ID,
    from: match.From.Address,
    to: match.To.map((t) => t.Address),
    subject: match.Subject,
  }
}

/** Poll Mailpit until a message with the given subject appears, or time out. */
export async function waitForMailpitMessage(
  mailpitUrl: string,
  subject: string,
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<MailpitMessage> {
  const timeoutMs = opts.timeoutMs ?? 20_000
  const intervalMs = opts.intervalMs ?? 1000
  const start = performance.now()
  while (performance.now() - start < timeoutMs) {
    const found = await findMailpitMessageBySubject(mailpitUrl, subject)
    if (found) return found
    await sleep(intervalMs)
  }
  throw new Error(
    `no message with subject "${subject}" appeared in Mailpit within ${Math.round(timeoutMs / 1000)}s`,
  )
}

/** Poll `email_events` for a given type (`opened`/`clicked`) until it appears, or time out. */
export async function waitForEmailEvent(
  client: Client,
  emailId: string,
  eventType: 'opened' | 'clicked',
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 15_000
  const intervalMs = opts.intervalMs ?? 1000
  const start = performance.now()
  while (performance.now() - start < timeoutMs) {
    const events = unwrap(
      await getEmailEvents({ client, path: { id: emailId } }),
      'getEmailEvents',
    )
    if (events.some((e) => e.event_type === eventType)) return
    await sleep(intervalMs)
  }
  throw new Error(
    `no "${eventType}" event recorded for email ${emailId} within ${Math.round(timeoutMs / 1000)}s`,
  )
}

export interface KvEnableResult {
  alreadyEnabled: boolean
}

/**
 * kv-storage is a platform-wide singleton (one shared Redis container for the
 * whole instance), not a per-project resource — unlike every other flow in
 * this file, callers must NOT disable it in teardown, since other concurrent
 * e2e runs or real users on a shared instance may depend on it staying up.
 *
 * Enables it if not already enabled+healthy, then polls until the underlying
 * container actually reports healthy. First-time enable pulls a Docker image
 * and waits for container health (can be slow on a cold image cache), so
 * `kvEnable`'s own response is not proof of usability — only a `healthy`
 * status read is.
 */
export async function ensureKvEnabled(
  client: Client,
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<KvEnableResult> {
  const initial = unwrap(await kvStatus({ client }), 'kvStatus')
  if (initial.enabled && initial.healthy) {
    return { alreadyEnabled: true }
  }
  if (!initial.enabled) {
    await kvEnable({ client, body: { persistence: true } })
  }
  const timeoutMs = opts.timeoutMs ?? 120_000
  const intervalMs = opts.intervalMs ?? 2000
  const start = performance.now()
  while (performance.now() - start < timeoutMs) {
    const s = unwrap(await kvStatus({ client }), 'kvStatus')
    if (s.enabled && s.healthy) return { alreadyEnabled: false }
    await sleep(intervalMs)
  }
  throw new Error(`kv-storage did not become enabled+healthy within ${Math.round(timeoutMs / 1000)}s`)
}

export interface BlobEnableResult {
  alreadyEnabled: boolean
}

/**
 * Blob storage is a platform-wide RustFS (S3-compatible) singleton, same
 * shape as kv-storage's shared Redis container -- callers must NOT disable
 * it in teardown, since other concurrent e2e runs or real users on a shared
 * instance may depend on it staying up. First-time enable pulls a Docker
 * image and waits for container health, so `blobEnable`'s own response is
 * not proof of usability -- only a `healthy` status read is.
 */
export async function ensureBlobEnabled(
  client: Client,
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<BlobEnableResult> {
  const initial = unwrap(await blobStatus({ client }), 'blobStatus')
  if (initial.enabled && initial.healthy) {
    return { alreadyEnabled: true }
  }
  if (!initial.enabled) {
    await blobEnable({ client, body: {} })
  }
  const timeoutMs = opts.timeoutMs ?? 120_000
  const intervalMs = opts.intervalMs ?? 2000
  const start = performance.now()
  while (performance.now() - start < timeoutMs) {
    const s = unwrap(await blobStatus({ client }), 'blobStatus')
    if (s.enabled && s.healthy) return { alreadyEnabled: false }
    await sleep(intervalMs)
  }
  throw new Error(`blob storage did not become enabled+healthy within ${Math.round(timeoutMs / 1000)}s`)
}

export interface DockerCommandResult {
  code: number
  stdout: string
  stderr: string
}

/**
 * Run a `docker` subcommand against a container by name and collect its
 * output. Used by HA/failover scenarios to simulate a real outage (`stop`/
 * `kill`) and independently confirm container state (`inspect`) via a side
 * channel the platform API can't fake -- Docker itself, not a DB row.
 */
export async function runDockerContainerCommand(
  action: 'stop' | 'start' | 'kill' | 'inspect',
  containerName: string,
  extraArgs: string[] = [],
): Promise<DockerCommandResult> {
  const args = action === 'inspect' ? ['inspect', ...extraArgs, containerName] : [action, ...extraArgs, containerName]
  const proc = Bun.spawn(['docker', ...args], { stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { code, stdout, stderr }
}

/** `docker inspect -f '{{.State.Status}}' <name>` -- 'running', 'exited', etc. Throws if the container doesn't exist. */
export async function dockerContainerStatus(containerName: string): Promise<string> {
  const res = await runDockerContainerCommand('inspect', containerName, ['-f', '{{.State.Status}}'])
  if (res.code !== 0) {
    throw new Error(`docker inspect ${containerName} failed: ${res.stderr.trim()}`)
  }
  return res.stdout.trim()
}

/** Best-effort teardown of the email-provider/domain resources this flow created. Never throws. */
export async function teardownEmailResources(
  client: Client,
  opts: { providerId?: number; domainId?: number },
): Promise<string[]> {
  const errors: string[] = []
  if (opts.domainId) {
    try {
      await deleteEmailDomain({ client, path: { id: opts.domainId } })
    } catch (e) {
      errors.push(`deleteEmailDomain(${opts.domainId}): ${(e as Error).message}`)
    }
  }
  if (opts.providerId) {
    try {
      await deleteEmailProvider({ client, path: { id: opts.providerId } })
    } catch (e) {
      errors.push(`deleteEmailProvider(${opts.providerId}): ${(e as Error).message}`)
    }
  }
  return errors
}

/**
 * Mirror of `PostgresService::normalize_database_name`
 * (`crates/temps-providers/src/externalsvc/postgres.rs`): lowercase,
 * non-alphanumeric -> `_`, `db_`-prefix if it'd start with a digit, capped
 * at 63 bytes (Postgres identifier limit).
 *
 * A linked postgres service does NOT hand a deployed app the database named
 * at service-creation time -- linking auto-provisions (and injects
 * `POSTGRES_URL` for) a SEPARATE per-project-per-environment database named
 * `normalize_database_name("{project_slug}_{environment_name}")` (see
 * `PostgresService::get_runtime_env_vars`), e.g. project slug
 * `my-app` + environment `production` -> database `my_app_production`.
 * Anything reading the actual table data back (not just calling the
 * deployed app's own HTTP API) needs this exact name, not the service's
 * own `database` parameter.
 */
export function normalizePostgresDatabaseName(name: string): string {
  let normalized = name.toLowerCase().replace(/[^a-z0-9]/g, '_')
  if (/^[0-9]/.test(normalized)) {
    normalized = `db_${normalized}`
  }
  return normalized.slice(0, 63)
}
